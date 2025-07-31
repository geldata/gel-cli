use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use clap::ValueHint;

use crate::branding::MANIFEST_FILE_DISPLAY_NAME;
use crate::commands::ExitCode;
use crate::options::CloudOptions;
use crate::portable::exit_codes;
use crate::instance::destroy;
use crate::project;
use crate::print::{self, Highlight, msg};
use crate::question;

use super::get_stash_path;

pub fn run(options: &Command, opts: &crate::options::Options) -> anyhow::Result<()> {
    let Some(project) = project::find_project(options.project_dir.as_deref())? else {
        anyhow::bail!("`{MANIFEST_FILE_DISPLAY_NAME}` not found, unable to unlink instance.");
    };
    let canon = dunce::canonicalize(&project.root)
        .with_context(|| format!("failed to canonicalize dir {:?}", project.root))?;
    let stash_path = get_stash_path(&canon)?;

    if stash_path.exists() {
        if options.destroy_server_instance {
            let inst = project::instance_name(&stash_path)?;
            if !options.non_interactive {
                let q = question::Confirm::new_dangerous(format!(
                    "Do you really want to unlink \
                             and delete instance {inst}?"
                ));
                if !q.ask()? {
                    print::error!("Canceled.");
                    return Ok(());
                }
            }
            let inst_name = inst.to_string();
            let mut project_dirs = project::find_project_dirs_by_instance(&inst_name)?;
            if project_dirs.len() > 1 {
                project_dirs
                    .iter()
                    .position(|d| d == &stash_path)
                    .map(|pos| project_dirs.remove(pos));
                destroy::print_warning(&inst_name, &project_dirs);
                Err(ExitCode::new(exit_codes::NEEDS_FORCE))?;
            }
            if options.destroy_server_instance {
                destroy::force_by_name(&inst, opts)?;
            }
        } else {
            match fs::read_to_string(stash_path.join("instance-name")) {
                Ok(name) => {
                    msg!("Unlinking instance {}", name.emphasized());
                }
                Err(e) => {
                    print::error!("Cannot read instance name: {e}");
                    eprintln!("Removing project configuration directory...");
                }
            };
        }
        fs::remove_dir_all(&stash_path)?;
    } else {
        log::warn!("no project directory exists");
    }
    Ok(())
}

#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Explicitly set a root directory for the project
    #[arg(long, value_hint=ValueHint::DirPath)]
    pub project_dir: Option<PathBuf>,

    /// If specified, the associated EdgeDB instance is destroyed
    /// using `edgedb instance destroy`.
    #[arg(long, short = 'D')]
    pub destroy_server_instance: bool,

    /// Unlink in in non-interactive mode (accepting all defaults)
    #[arg(long)]
    pub non_interactive: bool,
}
