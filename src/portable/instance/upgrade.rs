use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

use anyhow::Context;
use const_format::concatcp;
use fn_error_context::context;
use gel_cli_derive::IntoArgs;
use gel_cli_instance::cloud::CloudInstanceUpgrade;

use crate::branding::{BRANDING, BRANDING_CLI_CMD, BRANDING_CLOUD, QUERY_TAG};
use crate::commands::{self, ExitCode};
use crate::connect::{Connection, Connector};
use crate::options::{CloudOptions, InstanceOptionsLegacy};
use crate::portable::exit_codes;
use crate::portable::instance::control;
use crate::portable::instance::create;
use crate::portable::local::{InstallInfo, InstanceInfo, Paths, write_json};
use crate::portable::options::InstanceName;
use crate::portable::project;
use crate::portable::repository::{self, Channel, PackageInfo, Query, QueryOptions};
use crate::portable::server::install;
use crate::portable::ver;
use crate::portable::windows;
use crate::print::{self, Highlight, msg};
use crate::question;
use crate::{cloud, credentials};

pub fn run(cmd: &Command, opts: &crate::options::Options) -> anyhow::Result<()> {
    match cmd.instance_opts.instance()? {
        InstanceName::Local(name) => upgrade_local_cmd(cmd, &name),
        InstanceName::Cloud {
            org_slug: org,
            name,
        } => upgrade_cloud_cmd(cmd, &org, &name, opts),
    }
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Upgrade specified instance to latest /version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_testing", "to_nightly", "to_channel",
    ])]
    pub to_latest: bool,

    /// Upgrade specified instance to a specified version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_testing", "to_latest", "to_nightly", "to_channel",
    ])]
    pub to_version: Option<ver::Filter>,

    /// Upgrade specified instance to latest nightly version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_latest", "to_testing", "to_channel",
    ])]
    pub to_nightly: bool,

    /// Upgrade specified instance to latest testing version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_latest", "to_nightly", "to_channel",
    ])]
    pub to_testing: bool,

    /// Upgrade specified instance to latest version in the channel.
    #[arg(long, value_enum)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_latest", "to_nightly", "to_testing",
    ])]
    pub to_channel: Option<Channel>,

    #[command(flatten)]
    pub instance_opts: InstanceOptionsLegacy,

    /// Verbose output.
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Force upgrade even if there is no new version.
    #[arg(long)]
    pub force: bool,

    /// Force dump-restore during upgrade even if version is compatible.
    ///
    /// Used by `project upgrade --force`.
    #[arg(long, hide = true)]
    pub force_dump_restore: bool,

    /// Do not ask questions. Assume user wants to upgrade instance.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct UpgradeMeta {
    pub source: ver::Build,
    pub target: ver::Build,
    #[serde(with = "humantime_serde")]
    pub started: SystemTime,
    pub pid: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct BackupMeta {
    #[serde(with = "humantime_serde")]
    pub timestamp: SystemTime,
}

#[derive(Debug, Clone)]
pub enum UpgradeAction {
    None,
    Upgraded,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct UpgradeResult {
    pub action: UpgradeAction,
    pub prior_version: ver::Specific,
    pub requested_version: ver::Specific,
    pub available_upgrade: Option<ver::Specific>,
}

pub fn print_project_upgrade_command(
    version: &Query,
    current_project: &Option<PathBuf>,
    project_dir: &Path,
) {
    eprintln!(
        "  {BRANDING_CLI_CMD} project upgrade {}{}",
        match version.channel {
            Channel::Stable =>
                if let Some(filt) = &version.version {
                    format!("--to-version={filt}")
                } else {
                    "--to-latest".into()
                },
            Channel::Nightly => "--to-nightly".into(),
            Channel::Testing => "--to-testing".into(),
        },
        if current_project.as_ref().is_some_and(|p| p == project_dir) {
            "".into()
        } else {
            format!(" --project-dir '{}'", project_dir.display())
        }
    );
}

fn check_project(name: &str, force: bool, ver_query: &Query) -> anyhow::Result<()> {
    let project_dirs = project::find_project_dirs_by_instance(name)?;
    if project_dirs.is_empty() {
        return Ok(());
    }

    project::print_instance_in_use_warning(name, &project_dirs);

    if force {
        eprintln!(
            "To update the project{} after the instance upgrade, run:",
            if project_dirs.len() > 1 { "s" } else { "" }
        );
    } else {
        eprintln!("To continue with the upgrade, run:");
    }
    for pd in project_dirs {
        let pd = project::read_project_path(&pd)?;
        print_project_upgrade_command(ver_query, &None, &pd);
    }
    if !force {
        anyhow::bail!("Upgrade aborted.");
    }
    Ok(())
}

fn upgrade_local_cmd(cmd: &Command, name: &str) -> anyhow::Result<()> {
    let inst = InstanceInfo::read(name)?;
    let inst_ver = inst.get_version()?.specific();
    let (ver_query, ver_option) = Query::from_options(
        repository::QueryOptions {
            stable: cmd.to_latest,
            nightly: cmd.to_nightly,
            testing: cmd.to_testing,
            channel: cmd.to_channel,
            version: cmd.to_version.as_ref(),
        },
        || Query::from_version(&inst_ver),
    )?;
    check_project(name, cmd.force, &ver_query)?;

    if cfg!(windows) {
        return windows::upgrade(cmd, name);
    }

    let pkg = repository::get_server_package(&ver_query)?
        .context("no package found according to your criteria")?;
    let pkg_ver = pkg.version.specific();

    if pkg_ver <= inst_ver && !cmd.force {
        msg!(
            "Latest version found {}, current instance version is {}. Already up to date.",
            pkg.version.to_string(),
            inst.get_version()?.to_string().emphasized()
        );
        return Ok(());
    }
    ver::print_version_hint(&pkg_ver, &ver_query);

    let inst = InstanceInfo::read(name)?;
    // When force is used we might upgrade to the same version, so
    // we rely on presence of the version specifying options instead to
    // define how we want upgrade to be performed. This is mostly useful
    // for tests.
    if inst_ver.is_compatible(&pkg_ver) && !(cmd.force && ver_option) && !cmd.force_dump_restore {
        upgrade_compatible(inst, pkg)
    } else {
        upgrade_incompatible(inst, pkg, cmd.non_interactive)
    }
}

fn upgrade_cloud_cmd(
    cmd: &Command,
    org: &str,
    name: &str,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    let (query, _) = Query::from_options(
        QueryOptions {
            nightly: cmd.to_nightly,
            testing: cmd.to_testing,
            channel: cmd.to_channel,
            version: cmd.to_version.as_ref(),
            stable: cmd.to_latest,
        },
        || anyhow::Ok(Query::stable()),
    )?;

    let client = cloud::client::CloudClient::new(&opts.cloud_options)?;
    client.ensure_authenticated()?;

    let _inst_name = format!("{org}/{name}");
    let inst_name = _inst_name.emphasized();

    let result = upgrade_cloud(org, name, &query, &client, cmd.force, |target_ver| {
        let target_ver_str = target_ver.to_string();
        ver::print_version_hint(target_ver, &query);
        if !cmd.non_interactive {
            question::Confirm::new(format!(
                "This will upgrade {inst_name} to version {target_ver_str}.\
                    \nConfirm?",
            ))
            .ask()
        } else {
            Ok(true)
        }
    })?;

    let target_ver_str = result.requested_version.to_string();

    match result.action {
        UpgradeAction::Upgraded => {
            msg!(
                "{BRANDING_CLOUD} instance {inst_name} has been successfully \
                upgraded to version {target_ver_str}."
            );
        }
        UpgradeAction::Cancelled => {
            msg!("Canceled.");
        }
        UpgradeAction::None => {
            msg!(
                "Already up to date.\nRequested upgrade version is {}, current instance version is {}.",
                target_ver_str.emphasized(),
                result.prior_version.to_string().emphasized()
            );
        }
    }

    Ok(())
}

pub fn upgrade_cloud(
    org: &str,
    name: &str,
    to_version: &Query,
    client: &cloud::client::CloudClient,
    force: bool,
    confirm: impl FnOnce(&ver::Specific) -> anyhow::Result<bool>,
) -> anyhow::Result<UpgradeResult> {
    let inst = cloud::ops::find_cloud_instance_by_name(name, org, client)?
        .ok_or_else(|| anyhow::anyhow!("instance not found"))?;

    let target_ver = cloud::versions::get_version(to_version, client)?;
    let inst_ver = ver::Specific::from_str(&inst.version)?;

    if target_ver <= inst_ver && !force {
        Ok(UpgradeResult {
            action: UpgradeAction::None,
            prior_version: inst_ver,
            requested_version: target_ver,
            available_upgrade: None,
        })
    } else if !confirm(&target_ver)? {
        Ok(UpgradeResult {
            action: UpgradeAction::Cancelled,
            prior_version: inst_ver,
            requested_version: target_ver,
            available_upgrade: None,
        })
    } else {
        let request = CloudInstanceUpgrade {
            version: target_ver.to_string(),
            force,
        };

        cloud::ops::upgrade_cloud_instance(client, org, name, request)?;

        Ok(UpgradeResult {
            action: UpgradeAction::Upgraded,
            prior_version: inst_ver,
            requested_version: target_ver,
            available_upgrade: None,
        })
    }
}

pub fn upgrade_compatible(mut inst: InstanceInfo, pkg: PackageInfo) -> anyhow::Result<()> {
    msg!(
        "Upgrading to a minor version {}",
        pkg.version.to_string().emphasized()
    );
    let install = install::package(&pkg).context(concatcp!("error installing ", BRANDING))?;
    inst.installation = Some(install);

    let metapath = inst.data_dir()?.join("instance_info.json");
    write_json(&metapath, "new instance metadata", &inst)?;

    create::create_service(&inst)
        .map_err(|e| {
            log::warn!("Error running {BRANDING} as a service: {e:#}");
        })
        .ok();
    control::do_restart(&inst)?;
    msg!(
        "Instance {} successfully upgraded to {}",
        inst.name.emphasized(),
        pkg.version.to_string().emphasized()
    );
    Ok(())
}

pub fn upgrade_incompatible(
    mut inst: InstanceInfo,
    pkg: PackageInfo,
    non_interactive: bool,
) -> anyhow::Result<()> {
    msg!(
        "Upgrading to a major version {}",
        pkg.version.to_string().emphasized()
    );

    let old_version = inst.get_version()?.clone();

    let install = install::package(&pkg).context(concatcp!("error installing ", BRANDING))?;

    let paths = Paths::get(&inst.name)?;
    dump_and_stop(&inst, &paths.dump_path)?;

    backup(&inst, &install, &paths)?;

    inst.installation = Some(install);

    if old_version.specific().major <= 4 && pkg.version.specific().major >= 5 {
        let dump_files = fs::read_dir(&paths.dump_path)?;

        let mut has_edgedb_dump = false;
        let mut has_main_dump = false;

        for file in dump_files.flatten() {
            has_edgedb_dump |= file.file_name() == "edgedb.dump";
            has_main_dump |= file.file_name() == "main.dump";
        }

        if has_main_dump {
            print::warn!("The database 'main' will now become the default database");
        } else if has_edgedb_dump
            && (non_interactive
                || question::Confirm::new(
                    "Would you like to rename the database 'edgedb' to 'main'?",
                )
                .default(true)
                .ask()?)
        {
            // print info about the rename for non-prompt
            if non_interactive {
                eprintln!("Renaming 'edgedb' to 'main'");
            }

            fs::rename(
                paths.dump_path.join("edgedb.dump"),
                paths.dump_path.join("main.dump"),
            )?;

            if let Ok(mut creds) = credentials::read_sync(&paths.credentials) {
                if creds.database == Some("edgedb".to_string()) {
                    log::info!("Updating credentials file {:?}", &paths.credentials);
                    creds.database = Some("main".to_string());
                    creds.branch = Some("main".to_string());
                    credentials::write(&paths.credentials, &creds)?;
                }
            }
        }
    }

    reinit_and_restore(&inst, &paths).map_err(|e| {
        print::error!("{e:#}");
        eprintln!(
            "To undo run:\n  {BRANDING_CLI_CMD} instance revert -I {:?}",
            inst.name
        );
        ExitCode::new(exit_codes::NEEDS_REVERT)
    })?;

    fs::remove_file(&paths.upgrade_marker)
        .with_context(|| format!("removing {:?}", paths.upgrade_marker))?;

    create::create_service(&inst)
        .map_err(|e| {
            log::warn!("Error running {BRANDING} as a service: {e:#}");
        })
        .ok();
    control::do_restart(&inst)?;

    msg!(
        "Instance {} successfully upgraded to {}",
        inst.name.emphasized(),
        pkg.version.to_string().emphasized()
    );

    Ok(())
}

#[context("cannot dump {:?} -> {}", inst.name, path.display())]
pub fn dump_and_stop(inst: &InstanceInfo, path: &Path) -> anyhow::Result<()> {
    // in case not started for now
    msg!("Dumping the database...");
    log::info!("Ensuring instance is started");
    let res = control::do_start(inst);
    if let Err(err) = res {
        log::warn!(
            "Error starting service: {:#}. Trying to start manually.",
            err
        );
        control::ensure_runstate_dir(&inst.name)?;
        let mut cmd = control::get_server_cmd(inst, false)?;
        cmd.background_for(|| Ok(dump_instance(inst, path)))?;
    } else {
        block_on_dump_instance(inst, path)?;
        log::info!("Stopping instance before executable upgrade");
        control::do_stop(&inst.name)?;
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn block_on_dump_instance(inst: &InstanceInfo, destination: &Path) -> anyhow::Result<()> {
    dump_instance(inst, destination).await
}

#[context("error dumping instance")]
pub async fn dump_instance(inst: &InstanceInfo, destination: &Path) -> anyhow::Result<()> {
    use tokio::fs;

    let destination = Path::new(destination);
    log::info!("Dumping instance {:?}", inst.name);
    if fs::metadata(&destination).await.is_ok() {
        log::info!("Removing old dump at {}", destination.display());
        fs::remove_dir_all(&destination).await?;
    }
    let config = inst.admin_conn_params()?;
    let mut cli = Connection::connect(&config, QUERY_TAG).await?;
    let options = commands::Options {
        command_line: true,
        styler: None,
        conn_params: Connector::new(Ok(config)),
        instance_name: Some(InstanceName::Local(inst.name.clone())),
        branch: None,
    };
    commands::dump_all(
        &mut cli,
        &options,
        destination,
        true, /*include_secrets*/
    )
    .await?;
    Ok(())
}

fn backup(inst: &InstanceInfo, new_inst: &InstallInfo, paths: &Paths) -> anyhow::Result<()> {
    if paths.upgrade_marker.exists() {
        anyhow::bail!("Upgrade is already in progress");
    }
    write_json(
        &paths.upgrade_marker,
        "upgrade marker",
        &UpgradeMeta {
            source: inst.get_version()?.clone(),
            target: new_inst.version.clone(),
            started: SystemTime::now(),
            pid: std::process::id(),
        },
    )?;

    write_json(
        &paths.data_dir.join("backup.json"),
        "backup metadata",
        &BackupMeta {
            timestamp: SystemTime::now(),
        },
    )?;
    if paths.backup_dir.exists() {
        fs_err::remove_dir_all(&paths.backup_dir)?;
    }
    fs_err::rename(&paths.data_dir, &paths.backup_dir)?;

    Ok(())
}

#[context("cannot restore {:?}", inst.name)]
fn reinit_and_restore(inst: &InstanceInfo, paths: &Paths) -> anyhow::Result<()> {
    fs::create_dir_all(&paths.data_dir)
        .with_context(|| format!("cannot create {:?}", paths.data_dir))?;

    msg!("Restoring the database...");
    control::ensure_runstate_dir(&inst.name)?;
    let mut cmd = control::get_server_cmd(inst, false)?;
    control::self_signed_arg(&mut cmd, inst.get_version()?);
    cmd.background_for(|| {
        Ok(async {
            restore_instance(inst, &paths.dump_path).await?;
            log::info!(
                "Restarting instance {:?} to apply \
                   changes from `restore --all`",
                &inst.name
            );
            Ok(())
        })
    })?;

    let metapath = paths.data_dir.join("instance_info.json");
    write_json(&metapath, "new instance metadata", &inst)?;

    fs::copy(
        paths.backup_dir.join("edbtlscert.pem"),
        paths.data_dir.join("edbtlscert.pem"),
    )?;
    fs::copy(
        paths.backup_dir.join("edbprivkey.pem"),
        paths.data_dir.join("edbprivkey.pem"),
    )?;

    Ok(())
}

async fn restore_instance(inst: &InstanceInfo, path: &Path) -> anyhow::Result<()> {
    use crate::commands::parser::Restore;
    log::info!("Restoring instance {:?}", inst.name);
    let cfg = inst.admin_conn_params()?;
    let mut cli = Connection::connect(&cfg, QUERY_TAG).await?;

    let options = commands::Options {
        command_line: true,
        styler: None,
        conn_params: Connector::new(Ok(cfg)),
        instance_name: Some(InstanceName::Local(inst.name.clone())),
        branch: None,
    };
    commands::restore_all(
        &mut cli,
        &options,
        &Restore {
            path: path.into(),
            all: true,
            verbose: false,
            conn: None,
        },
    )
    .await?;
    Ok(())
}
