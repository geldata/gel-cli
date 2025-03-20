use anyhow::Context;
use gel_cli_derive::IntoArgs;

use crate::portable::local;
use crate::portable::repository::{Channel, Query, QueryOptions};
use crate::portable::ver;
use crate::table;

pub fn run(cmd: &Command) -> anyhow::Result<()> {
    // note this assumes that latest is set if no nightly and version
    let (query, _) = Query::from_options(
        QueryOptions {
            stable: cmd.latest,
            nightly: cmd.nightly,
            testing: false,
            channel: cmd.channel,
            version: cmd.version.as_ref(),
        },
        || {
            anyhow::bail!(
                "One of `--latest`, `--channel=`, \
                         `--version=` required"
            )
        },
    )?;
    let all = local::get_installed()?;
    let inst = all
        .into_iter()
        .filter(|item| query.matches(&item.version))
        .max_by_key(|item| item.version.specific())
        .context("cannot find installed packages maching your criteria")?;

    let item = cmd.get.as_deref().or(cmd.bin_path.then_some("bin-path"));
    if let Some(item) = item {
        match item {
            "bin-path" => {
                let path = inst.server_path()?;
                if cmd.json {
                    let path = path.to_str().context("cannot convert path to a string")?;
                    println!("{}", serde_json::to_string(path)?);
                } else {
                    println!("{}", path.display());
                }
            }
            "version" => {
                let version = &inst.version;
                if cmd.json {
                    println!("{}", serde_json::to_string(version)?);
                } else {
                    println!("{version}");
                }
            }
            _ => unreachable!(),
        }
    } else if cmd.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&JsonInfo {
                version: &inst.version,
                binary_path: inst.server_path()?.to_str(),
            })?
        )
    } else {
        table::settings(&[
            ("Version", inst.version.to_string()),
            ("Binary path", inst.server_path()?.display().to_string()),
        ]);
    }
    Ok(())
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    /// Display only the server binary path (shortcut to `--get bin-path`).
    #[arg(long)]
    pub bin_path: bool,
    /// Output in JSON format.
    #[arg(long)]
    pub json: bool,

    // Display info for latest version.
    #[arg(long)]
    #[arg(conflicts_with_all=&["channel", "version", "nightly"])]
    pub latest: bool,
    // Display info for nightly version.
    #[arg(long)]
    #[arg(conflicts_with_all=&["channel", "version", "latest"])]
    pub nightly: bool,
    // Display info for specific version.
    #[arg(long)]
    #[arg(conflicts_with_all=&["nightly", "channel", "latest"])]
    pub version: Option<ver::Filter>,
    // Display info for specific channel.
    #[arg(long, value_enum)]
    #[arg(conflicts_with_all=&["nightly", "version", "latest"])]
    pub channel: Option<Channel>,

    /// Get specific value:
    ///
    /// * `bin-path` -- Path to the server binary
    /// * `version` -- Server version
    #[arg(long, value_parser=["bin-path", "version"])]
    pub get: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "kebab-case")]
struct JsonInfo<'a> {
    version: &'a ver::Build,
    binary_path: Option<&'a str>,
}
