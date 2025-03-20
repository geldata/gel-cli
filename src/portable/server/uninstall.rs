use fs_err as fs;
use gel_cli_derive::IntoArgs;
use std::collections::BTreeMap;

use crate::commands::ExitCode;
use crate::platform::{data_dir, portable_dir, tmp_file_path};
use crate::portable::exit_codes;
use crate::portable::instance::status;
use crate::portable::local;
use crate::portable::local::InstanceInfo;
use crate::portable::repository::{Channel, Query};
use crate::portable::ver;
use crate::print::{self, Highlight, msg};

pub fn run(options: &Command) -> anyhow::Result<()> {
    let mut candidates = local::get_installed()?;
    if options.nightly {
        candidates.retain(|cand| cand.version.is_nightly());
    }
    if let Some(channel) = options.channel {
        let query = Query::from(channel);
        candidates.retain(|cand| query.matches(&cand.version));
    }
    if let Some(ver) = &options.version {
        if let Ok(ver) = ver.parse::<ver::Filter>() {
            candidates.retain(|cand| ver.matches(&cand.version));
        } else if let Ok(ver) = ver.parse::<ver::Specific>() {
            candidates.retain(|cand| ver == cand.version.specific());
        } else if let Ok(ver) = ver.parse::<ver::Build>() {
            candidates.retain(|cand| ver == cand.version);
        } else {
            anyhow::bail!("cannot parse version {:?}", ver);
        }
    }
    let mut used_versions = BTreeMap::new();
    let data_dir = data_dir()?;
    if data_dir.exists() {
        for pair in status::list_local(&data_dir)? {
            let (name, _) = pair?;
            if let Some(info) = InstanceInfo::try_read(&name)? {
                used_versions.insert(info.get_version()?.specific(), info.name);
            }
        }
    }
    let mut all = true;
    candidates.retain(|cand| {
        if let Some(inst_name) = used_versions.get(&cand.version.specific()) {
            if !options.unused {
                log::warn!("Version {} is used by {:?}", cand.version, inst_name);
            }
            all = false;
            false
        } else {
            true
        }
    });
    let mut uninstalled = 0;
    for cand in candidates {
        log::info!("Uninstalling {}", cand.version);
        let path = portable_dir()?.join(cand.version.specific().to_string());
        let tmp_dir = tmp_file_path(&path);
        if tmp_dir.exists() {
            fs::remove_dir_all(&tmp_dir)?;
        }
        fs::rename(path, &tmp_dir)?;
        fs::remove_dir_all(&tmp_dir)?;
        uninstalled += 1;
    }

    if !all && !options.unused {
        msg!(
            "Uninstalled {} versions.",
            uninstalled.to_string().emphasized()
        );
        print::error!("some instances are in use. See messages above.");
        Err(ExitCode::new(exit_codes::PARTIAL_SUCCESS))?;
    } else if uninstalled > 0 {
        msg!(
            "Successfully uninstalled {} versions.",
            uninstalled.to_string().emphasized()
        );
    } else {
        print::success!("Nothing to uninstall.")
    }
    Ok(())
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    /// Uninstall all versions.
    #[arg(long)]
    pub all: bool,
    /// Uninstall unused versions.
    #[arg(long)]
    pub unused: bool,
    /// Uninstall nightly versions.
    #[arg(long, conflicts_with_all=&["channel"])]
    pub nightly: bool,
    /// Uninstall specific version.
    pub version: Option<String>,
    /// Uninstall only versions from a specific channel.
    #[arg(long, conflicts_with_all=&["nightly"])]
    #[arg(value_enum)]
    pub channel: Option<Channel>,
    /// Increase verbosity.
    #[arg(short = 'v', long)]
    pub verbose: bool,
}
