use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use gel_tokio::InstanceName;

use crate::branding::{BRANDING_CLI_CMD, BRANDING_CLOUD};
use crate::credentials;
use crate::hint::HintExt;
use crate::options::InstanceOptionsLegacy;
use crate::portable::instance::destroy::with_projects;
use crate::portable::local::InstanceInfo;
use crate::portable::project;

pub fn run(cmd: &Command) -> anyhow::Result<()> {
    let name = match cmd.instance_opts.instance()? {
        InstanceName::Local(name) => name,
        inst_name => {
            return Err(anyhow::anyhow!(
                "cannot unlink {BRANDING_CLOUD} instance {}.",
                inst_name
            ))
            .with_hint(|| {
                format!("use `{BRANDING_CLI_CMD} instance destroy -I {inst_name}` to remove the instance")
            })?;
        }
    };
    let inst = InstanceInfo::try_read(&name)?;
    if inst.is_some() {
        return Err(anyhow::anyhow!("cannot unlink local instance {:?}.", name)
            .with_hint(|| {
                format!(
                    "use `{BRANDING_CLI_CMD} instance destroy -I {name}` to remove the instance"
                )
            })
            .into());
    }
    with_projects(&name, cmd.force, print_warning, || {
        let path = credentials::path(&name)?;
        fs::remove_file(&path)
            .with_context(|| format!("Credentials for {name} missing from {path:?}"))
    })?;
    Ok(())
}

#[derive(clap::Args, Clone, Debug)]
pub struct Command {
    #[command(flatten)]
    pub instance_opts: InstanceOptionsLegacy,

    /// Force destroy even if instance is referred to by a project.
    #[arg(long)]
    pub force: bool,
}

pub fn print_warning(name: &str, project_dirs: &[PathBuf]) {
    project::print_instance_in_use_warning(name, project_dirs);
    eprintln!("If you really want to unlink the instance, run:");
    eprintln!("  {BRANDING_CLI_CMD} instance unlink -I {name:?} --force");
}
