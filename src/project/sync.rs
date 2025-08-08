use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::ValueHint;
use edgeql_parser::helpers::quote_name;
use gel_tokio::InstanceName;

use crate::branding::{
    BRANDING_CLI_CMD, BRANDING_LOCAL_CONFIG_FILE, BRANDING_SCHEMA_FILE_EXT,
    MANIFEST_FILE_DISPLAY_NAME,
};
use crate::cloud::client::CloudClient;
use crate::commands::{ExitCode, Options};
use crate::connect::Connector;
use crate::hint::{HintExt, HintedError};
use crate::instance::control;
use crate::portable::windows;
use crate::print::AsRelativeToCurrentDir;
use crate::{migrations, msg, print, project, question};

#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    /// Explicitly set a root directory for the project
    #[arg(long, value_hint=ValueHint::DirPath)]
    pub project_dir: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
pub async fn run(options: &Command, opts: &crate::options::Options) -> anyhow::Result<()> {
    let client = CloudClient::new(&opts.cloud_options)?;
    let inst = {
        let project = project::load_ctx(options.project_dir.as_deref(), true).await?.ok_or_else(|| {
            anyhow::anyhow!(
                "`{MANIFEST_FILE_DISPLAY_NAME}` not found, unable to perform this action without an initialized project."
            )
        })?;
        let stash_dir = project::get_stash_path(&project.location.root)?;
        if !stash_dir.exists() {
            anyhow::bail!("No instance initialized.");
        }
        let instance_name = project::instance_name(&stash_dir)?;
        let schema_dir = project.resolve_schema_dir()?;
        project::Handle::probe(&instance_name, &project.location.root, &schema_dir, &client)?
    };
    sync(&inst, true, false).await?;

    Ok(())
}

async fn sync(
    inst: &project::Handle<'_>,
    ask_for_running: bool,
    skip_hooks: bool,
) -> anyhow::Result<()> {
    #[derive(Clone, Copy)]
    enum Action {
        Retry,
        Service,
        Run,
    }

    let mut conn = loop {
        match Box::pin(inst.get_default_connection()).await {
            Ok(conn) => break conn,
            Err(e) if ask_for_running && inst.instance.is_local() => {
                print::error!("{e}");
                let mut q = question::Numeric::new(format!(
                    "Cannot connect to instance {:?}. Options:",
                    inst.name,
                ));
                q.option("Start the service (if possible).", Action::Service);
                q.option(
                    "Start in the foreground, \
                          run sync and shut down.",
                    Action::Run,
                );
                q.option(
                    "Instance has been started manually, retry connect",
                    Action::Retry,
                );
                match q.async_ask().await? {
                    Action::Service => match start(inst) {
                        Ok(()) => continue,
                        Err(e) => {
                            print::error!("{e}");
                            continue;
                        }
                    },
                    Action::Run => {
                        return run_and_sync(inst, skip_hooks);
                    }
                    Action::Retry => continue,
                }
            }
            Err(e) => return Err(e)?,
        };
    };
    if let Some(database) = &inst.database {
        let name = quote_name(database);
        if let Err(e) = conn.execute(&format!("CREATE DATABASE {name}"), &()).await {
            if !e.is::<gel_errors::DuplicateDatabaseDefinitionError>() {
                return Err(e)?;
            }
        }
        conn = Box::pin(inst.get_connection()).await?;
    }

    let cfg = migrations::options::MigrationConfig {
        schema_dir: Some(inst.project_dir.join(&inst.schema_dir)),
    };
    let options = Options {
        command_line: true,
        styler: None,
        conn_params: Connector::new(inst.get_builder()?.build().map_err(Into::into)),
        instance_name: Some(InstanceName::from_str(&inst.name)?),
        skip_hooks,
    };

    msg!("1. Applying migrations...");
    migrations::apply::run(
        &migrations::apply::Command {
            cfg: cfg.clone(),
            quiet: true,
            to_revision: None,
            dev_mode: false,
            single_transaction: false,
            no_index_build: false,
            conn: None,
        },
        &mut conn,
        &options,
        false,
    )
    .await?;
    msg!("Done.");

    msg!("2. Checking if schema is up to date...");
    match migrations::create::run_inner(
        &migrations::create::Command {
            cfg,
            squash: false,
            non_interactive: true,
            allow_unsafe: false,
            allow_empty: false,
            debug_print_queries: false,
            debug_print_err: false,
            quiet: true,
        },
        &mut conn,
        &options,
    )
    .await
    {
        Ok(migration_file) => {
            print::warn!(
                "Please check the generated migration file {migration_file} and \
                run `{BRANDING_CLI_CMD} sync` again to apply it.",
            );
            return Err(ExitCode::new(1))?;
        }
        Err(e) => {
            if let Some(code) = e.downcast_ref::<ExitCode>() {
                if code.code() == 4 {
                    // No schema changes detected, good to go on
                    msg!("Done.");
                } else {
                    // Other errors
                    return Err(ExitCode::new(2))?;
                }
            } else {
                return Err(e).with_hint(|| {
                    format!("run `{BRANDING_CLI_CMD} migration create` manually.")
                })?;
            }
        }
    }

    msg!("3. Applying config...");
    match project::config::apply(&inst.project_dir, true).await {
        Ok(true) => {
            print::success!("Project is now in sync.")
        }
        Ok(false) => {
            msg!(
                "No config to apply, run `{BRANDING_CLI_CMD} sync` again \
                after modifying `{BRANDING_LOCAL_CONFIG_FILE}`.",
            );
        }
        Err(err) => {
            let extensions_gel = inst
                .schema_dir
                .join(format!("extensions.{BRANDING_SCHEMA_FILE_EXT}"));
            let extension_name = maybe_enable_missing_extension(err, &inst.schema_dir)?;
            print::warn!(
                "Extension `{extension_name}` is required by the config. \
                        It's now enabled in {}, please run `{BRANDING_CLI_CMD} sync` again.",
                extensions_gel.as_relative().display()
            );
            return Err(ExitCode::new(1))?;
        }
    }
    Ok(())
}

pub fn maybe_enable_missing_extension(
    err: anyhow::Error,
    schema_dir: &Path,
) -> anyhow::Result<String> {
    let mut e = &err;
    if let Some(err) = err.downcast_ref::<HintedError>() {
        e = &err.error;
    }
    if let Some(project::config::MissingExtension(extension_name)) = e.downcast_ref() {
        let extensions_gel = schema_dir.join(format!("extensions.{BRANDING_SCHEMA_FILE_EXT}"));
        if extensions_gel.exists() {
            let content = std::fs::read_to_string(&extensions_gel)?;
            let pattern = format!("#using extension {extension_name};");
            let mut enabled = false;
            let uncommented = content
                .lines()
                .map(|line| {
                    if line.trim_start().starts_with(&pattern) {
                        enabled = true;
                        line.replacen("#using", "using", 1)
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if enabled {
                let tmp_file =
                    extensions_gel.with_extension(format!("{BRANDING_SCHEMA_FILE_EXT}.tmp"));
                std::fs::write(&tmp_file, uncommented)?;
                std::fs::rename(&tmp_file, &extensions_gel)?;
                return Ok(extension_name.clone());
            }
        }
    }
    Err(err)
}

fn run_and_sync(info: &project::Handle, skip_hooks: bool) -> anyhow::Result<()> {
    match &info.instance {
        project::InstanceKind::Portable(inst) => {
            control::ensure_runstate_dir(&info.name)?;
            let mut cmd = control::get_server_cmd(inst, false)?;
            cmd.background_for(|| Ok(sync(info, false, skip_hooks)))?;
            Ok(())
        }
        project::InstanceKind::Wsl => {
            let mut cmd = windows::server_cmd(&info.name, false)?;
            cmd.background_for(|| Ok(sync(info, false, skip_hooks)))?;
            Ok(())
        }
        project::InstanceKind::Remote => {
            anyhow::bail!(
                "remote instance not running, \
                          cannot run sync"
            );
        }
        project::InstanceKind::Cloud { .. } => todo!(),
    }
}

fn start(handle: &project::Handle) -> anyhow::Result<()> {
    match &handle.instance {
        project::InstanceKind::Portable(inst) => {
            control::do_start(inst)?;
            Ok(())
        }
        project::InstanceKind::Wsl => {
            windows::daemon_start(&handle.name)?;
            Ok(())
        }
        project::InstanceKind::Remote => {
            anyhow::bail!(
                "remote instance not running, \
                          cannot run sync"
            );
        }
        project::InstanceKind::Cloud { .. } => todo!(),
    }
}
