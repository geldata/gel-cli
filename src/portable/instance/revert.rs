use anyhow::Context;
use const_format::concatcp;
use fs_err as fs;
use gel_cli_derive::IntoArgs;
use gel_tokio::InstanceName;

use crate::branding::{BRANDING, BRANDING_CLOUD};
use crate::commands::ExitCode;
use crate::options::InstanceOptionsLegacy;
use crate::platform::tmp_file_path;
use crate::portable::exit_codes;
use crate::portable::instance::control;
use crate::portable::instance::create;
use crate::portable::instance::status::{BackupStatus, DataDirectory, instance_status};
use crate::portable::local::Paths;
use crate::portable::server::install;
use crate::print::{self, Highlight, msg};
use crate::process;
use crate::question;
use crate::{credentials, format};

pub fn run(options: &Command) -> anyhow::Result<()> {
    use BackupStatus::*;

    let instance = options.instance_opts.instance()?;

    let name = match &instance {
        InstanceName::Local(name) => {
            if cfg!(windows) {
                return crate::portable::windows::revert(options, name);
            } else {
                name
            }
        }
        InstanceName::Cloud { .. } => {
            print::error!("This operation is not yet supported on {BRANDING_CLOUD} instances.");
            return Err(ExitCode::new(1))?;
        }
    };
    let status = instance_status(name)?;
    let (backup_info, old_inst) = match status.backup {
        Absent => anyhow::bail!("cannot find backup directory to revert"),
        Exists {
            backup_meta: Err(e),
            ..
        } => anyhow::bail!("cannot read backup metadata: {}", e),
        Exists {
            data_meta: Err(e), ..
        } => anyhow::bail!("cannot read backup metadata: {}", e),
        Exists {
            backup_meta: Ok(b),
            data_meta: Ok(d),
        } => (b, d),
    };

    let old_version = old_inst.get_version()?;
    let current_version = status
        .instance
        .ok()
        .as_ref()
        .and_then(|i| i.installation.as_ref().map(|i| i.version.clone()));
    if let Some(current_version) = &current_version {
        msg!("Current {BRANDING} version: {current_version}");
    }
    msg!("Backup {BRANDING} version: {old_version}");
    msg!(
        "Backup timestamp: {} {}",
        humantime::format_rfc3339(backup_info.timestamp),
        format!("({})", format::done_before(backup_info.timestamp))
    );
    if !options.ignore_pid_check {
        match status.data_status {
            DataDirectory::Upgrading(Ok(up)) if process::exists(up.pid) => {
                msg!(
                    "Upgrade appears to still be in progress \
                    with pid {}",
                    up.pid.to_string().emphasized()
                );
                msg!("Run with `--ignore-pid-check` to override");
                Err(ExitCode::new(exit_codes::NEEDS_FORCE))?;
            }
            DataDirectory::Upgrading(_) => {
                msg!("Note: backup appears to be from a broken upgrade");
            }
            _ => {}
        }
    }
    if !options.no_confirm {
        eprintln!();
        msg!(
            "Currently stored data {} and overwritten by the backup.",
            "will be lost".emphasized()
        );
        let q = question::Confirm::new_dangerous("Do you really want to revert?");
        if !q.ask()? {
            print::error!("Canceled.");
            Err(ExitCode::new(exit_codes::NOT_CONFIRMED))?;
        }
    }

    if let Err(e) = control::do_stop(name) {
        print::error!("Error stopping service: {e:#}");
        if !options.no_confirm {
            let q = question::Confirm::new("Do you want to proceed?");
            if !q.ask()? {
                print::error!("Canceled.");
                Err(ExitCode::new(exit_codes::NOT_CONFIRMED))?;
            }
        }
    }

    install::specific(&old_version.specific())
        .context(concatcp!("error installing old ", BRANDING))?;

    let paths = Paths::get(name)?;
    let tmp_path = tmp_file_path(&paths.data_dir);
    fs::rename(&paths.data_dir, &tmp_path)?;
    fs::rename(&paths.old_backup_dir, &paths.data_dir)?;

    // If we're reverting from >=5.x to <=4.x, we need to rewrite the
    // credentials file to use the edgedb database _if_ the credentials are
    // pointing to the main branch and the edgedb branch was renamed as part of
    // the upgrade.
    if let Some(current_version) = current_version {
        if old_version.specific().major <= 4 && current_version.specific().major >= 5 {
            let dump_files = fs::read_dir(&paths.dump_path)?;

            let mut has_edgedb_dump = false;
            let mut has_main_dump = false;

            for file in dump_files.flatten() {
                has_edgedb_dump |= file.file_name() == "edgedb.dump";
                has_main_dump |= file.file_name() == "main.dump";
            }

            if !has_edgedb_dump && has_main_dump {
                if let Some(mut creds) = credentials::read(&instance)? {
                    creds.database = Some("edgedb".to_string());
                    creds.branch = Some("edgedb".to_string());
                    credentials::write(&instance, &creds)?;
                }
            }
        }
    }

    msg!("Starting {BRANDING} {old_version}...");

    let inst = old_inst;
    create::create_service(&inst)
        .map_err(|e| {
            log::warn!("Error running {BRANDING} as a service: {e:#}");
        })
        .ok();

    control::do_restart(&inst)?;
    msg!(
        "Instance {} is successfully reverted to {}",
        inst.name.as_str().emphasized(),
        inst.get_version()?.to_string().emphasized()
    );

    fs::remove_file(paths.data_dir.join("backup.json"))?;
    fs::remove_dir_all(&tmp_path)?;
    Ok(())
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    #[command(flatten)]
    pub instance_opts: InstanceOptionsLegacy,

    /// Do not check if upgrade is in progress.
    #[arg(long)]
    pub ignore_pid_check: bool,

    /// Do not ask for confirmation.
    #[arg(short = 'y', long)]
    pub no_confirm: bool,
}
