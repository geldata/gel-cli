use std::path::PathBuf;

use fs_err as fs;
use gel_cli_derive::IntoArgs;
use gel_tokio::InstanceName;

use crate::branding::{BRANDING_CLI_CMD, BRANDING_CLOUD};
use crate::commands::ExitCode;
use crate::locking::LockManager;
use crate::options::{CloudOptions, InstanceOptionsLegacy, Options};
use crate::portable::exit_codes;
use crate::portable::instance::control;
use crate::portable::local;
use crate::portable::project;
use crate::portable::windows;
use crate::print::{self, Highlight, msg};
use crate::{credentials, question};

pub fn run(options: &Command, opts: &Options) -> anyhow::Result<()> {
    let name = options.instance_opts.instance()?;
    let _lock = LockManager::lock_instance(&name)?;

    let name_str = name.to_string();
    with_projects(&name_str, options.force, print_warning, || {
        if !options.force && !options.non_interactive {
            let q = question::Confirm::new_dangerous(format!(
                "Do you really want to delete instance {name_str:?}? All data, dumps, configuration, backups and credentials will be permanently lost."
            ));
            if !q.ask()? {
                print::error!("Canceled.");
                return Err(ExitCode::new(exit_codes::NOT_CONFIRMED).into());
            }
        }
        match do_destroy(options, opts, &name) {
            Ok(()) => Ok(()),
            Err(e) if e.is::<InstanceNotFound>() => {
                print::error!("{e}");
                Err(ExitCode::new(exit_codes::INSTANCE_NOT_FOUND).into())
            }
            Err(e) => Err(e),
        }
    })?;
    if !options.quiet {
        msg!(
            "{} was successfully deleted.",
            format!("{name:#}").emphasized()
        );
    }
    Ok(())
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    #[command(flatten)]
    pub instance_opts: InstanceOptionsLegacy,

    /// Verbose output.
    #[arg(short = 'v', long, overrides_with = "quiet")]
    pub verbose: bool,

    /// Quiet output.
    #[arg(short = 'q', long, overrides_with = "verbose")]
    pub quiet: bool,

    /// Force destroy even if instance is referred to by a project.
    #[arg(long)]
    pub force: bool,

    /// Do not ask questions. Assume user wants to delete instance.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("instance not found")]
pub struct InstanceNotFound(#[source] pub anyhow::Error);

pub fn print_warning(name: &str, project_dirs: &[PathBuf]) {
    project::print_instance_in_use_warning(name, project_dirs);
    eprintln!("If you really want to destroy the instance, run:");
    eprintln!("  {BRANDING_CLI_CMD} instance destroy -I {name:?} --force");
}

pub fn with_projects(
    name: &str,
    force: bool,
    warn: impl FnOnce(&str, &[PathBuf]),
    f: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let project_dirs = project::find_project_dirs_by_instance(name)?;
    if !force && !project_dirs.is_empty() {
        warn(name, &project_dirs);
        Err(ExitCode::new(exit_codes::NEEDS_FORCE))?;
    }
    f()?;
    for dir in project_dirs {
        match project::read_project_path(&dir) {
            Ok(path) => eprintln!("Unlinking {}", path.display()),
            Err(_) => eprintln!("Cleaning {}", dir.display()),
        };
        fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

fn destroy_local(name: &str) -> anyhow::Result<bool> {
    let paths = local::Paths::get(name)?;
    log::debug!("Paths {:?}", paths);
    let mut found = false;
    match control::stop_and_disable(name) {
        Ok(f) => found = f,
        Err(e) if e.is::<InstanceNotFound>() => {}
        Err(e) => {
            log::warn!("Error unloading service: {:#}", e);
        }
    }
    if paths.runstate_dir.exists() {
        // Don't set 'found' if the runstate exists since we might have a lock
        // only
        log::info!("Removing runstate directory {:?}", paths.runstate_dir);
        fs::remove_dir_all(&paths.runstate_dir)?;
    }
    if paths.data_dir.exists() {
        found = true;
        log::info!("Removing data directory {:?}", paths.data_dir);
        fs::remove_dir_all(&paths.data_dir)?;
    }
    for path in &paths.service_files {
        if path.exists() {
            found = true;
            log::info!("Removing service file {:?}", path);
            fs::remove_file(path)?;
        }
    }
    if paths.old_backup_dir.exists() {
        found = true;
        log::info!("Removing backup directory {:?}", paths.old_backup_dir);
        fs::remove_dir_all(&paths.old_backup_dir)?;
    }
    if paths.backups_dir.exists() {
        found = true;
        log::info!("Removing backups directory {:?}", paths.backups_dir);
        fs::remove_dir_all(&paths.backups_dir)?;
    }
    if paths.dump_path.exists() {
        found = true;
        log::info!("Removing dump {:?}", paths.dump_path);
        fs::remove_dir_all(&paths.dump_path)?;
    }
    if paths.upgrade_marker.exists() {
        found = true;
        log::info!("Removing upgrade marker {:?}", paths.upgrade_marker);
        fs::remove_file(&paths.upgrade_marker)?;
    }

    Ok(found)
}

fn do_destroy(options: &Command, opts: &Options, instance: &InstanceName) -> anyhow::Result<()> {
    match instance {
        InstanceName::Local(name) => {
            let mut found = if cfg!(windows) {
                windows::destroy(options, name)?
            } else {
                destroy_local(name)?
            };
            if credentials::exists(instance)? {
                found = true;
                credentials::delete(instance)?;
            } else {
                // Only warn if we actually found any instance data.
                if found {
                    log::warn!("Credentials unexpectedly missing for {:#}", instance);
                }
            }
            if !found {
                if !windows::is_wrapped() {
                    msg!("{} Could not find {:#}", print::err_marker(), instance);
                }
                return Err(ExitCode::new(exit_codes::INSTANCE_NOT_FOUND).into());
            }
            Ok(())
        }
        InstanceName::Cloud(name) => {
            log::info!("Removing {name:#}");
            if let Err(e) = crate::cloud::ops::destroy_cloud_instance(name, &opts.cloud_options) {
                let msg = format!("Could not destroy {BRANDING_CLOUD} instance: {e:#}");
                if options.force {
                    print::warn!("{msg}");
                } else {
                    anyhow::bail!(msg);
                }
            }
            Ok(())
        }
    }
}

pub fn force_by_name(name: &InstanceName, options: &Options) -> anyhow::Result<()> {
    do_destroy(
        &Command {
            instance_opts: name.clone().into(),
            verbose: false,
            force: true,
            quiet: false,
            non_interactive: true,
            cloud_opts: options.cloud_options.clone(),
        },
        options,
        name,
    )
}
