use anyhow::Context;
use const_format::concatcp;
use fs_err as fs;
use gel_cli_derive::IntoArgs;

use crate::branding::{BRANDING, BRANDING_CLOUD};
use crate::commands::ExitCode;
use crate::format;
use crate::options::InstanceOptionsLegacy;
use crate::platform::tmp_file_path;
use crate::portable::exit_codes;
use crate::portable::instance::control;
use crate::portable::instance::create;
use crate::portable::instance::status::{BackupStatus, DataDirectory, instance_status};
use crate::portable::local::Paths;
use crate::portable::options::InstanceName;
use crate::portable::server::install;
use crate::print::{self, Highlight, msg};
use crate::process;
use crate::question;

pub fn run(options: &Command) -> anyhow::Result<()> {
    use BackupStatus::*;

    let name = match options.instance_opts.instance()? {
        InstanceName::Local(name) => {
            if cfg!(windows) {
                return crate::portable::windows::revert(options, &name);
            } else {
                name
            }
        }
        InstanceName::Cloud { .. } => {
            print::error!("This operation is not yet supported on {BRANDING_CLOUD} instances.");
            return Err(ExitCode::new(1))?;
        }
    };
    let status = instance_status(&name)?;
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
    msg!("{} version: {:?}", BRANDING, old_inst.get_version());
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

    if let Err(e) = control::do_stop(&name) {
        print::error!("Error stopping service: {e:#}");
        if !options.no_confirm {
            let q = question::Confirm::new("Do you want to proceed?");
            if !q.ask()? {
                print::error!("Canceled.");
                Err(ExitCode::new(exit_codes::NOT_CONFIRMED))?;
            }
        }
    }

    install::specific(&old_inst.get_version()?.specific())
        .context(concatcp!("error installing old ", BRANDING))?;

    let paths = Paths::get(&name)?;
    let tmp_path = tmp_file_path(&paths.data_dir);
    fs::rename(&paths.data_dir, &tmp_path)?;
    fs::rename(&paths.backup_dir, &paths.data_dir)?;

    let inst = old_inst;
    msg!("Starting {} {:?}...", BRANDING, inst.get_version());

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
