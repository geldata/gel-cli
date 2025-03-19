use std::collections::BTreeMap;
use std::path::PathBuf;

use edgedb_cli_derive::IntoArgs;

use crate::portable::local::{self, InstallInfo};
use crate::portable::repository::{Channel, PackageInfo, get_server_packages};
use crate::portable::ver;
use crate::table::{self, Cell, Row, Table};

pub fn run(cmd: &Command) -> Result<(), anyhow::Error> {
    let mut installed = local::get_installed()?;
    if cmd.installed_only {
        if cmd.json {
            print!(
                "{}",
                serde_json::to_string_pretty(
                    &installed
                        .into_iter()
                        .map(|v| JsonVersionInfo {
                            channel: Channel::from_version(&v.version.specific())
                                .unwrap_or(Channel::Nightly),
                            version: v.version.clone(),
                            installed: true,
                            debug_info: DebugInfo {
                                install: Some(DebugInstall::from(v)),
                                package: None,
                            },
                        })
                        .collect::<Vec<_>>()
                )?
            );
        } else {
            installed.sort_by(|a, b| a.version.specific().cmp(&b.version.specific()));
            print_table(installed.into_iter().map(|p| (p.version, true)));
        }
    } else {
        let mut version_set = BTreeMap::new();
        for package in all_packages() {
            version_set.insert(
                package.version.specific(),
                Pair {
                    package: Some(package),
                    install: None,
                },
            );
        }
        for install in installed {
            let _ = version_set
                .entry(install.version.specific())
                .or_insert_with(|| Pair {
                    package: None,
                    install: None,
                })
                .install
                .insert(install);
        }
        if cmd.json {
            print!(
                "{}",
                serde_json::to_string_pretty(
                    &version_set
                        .into_iter()
                        .map(|(ver, vp)| JsonVersionInfo {
                            channel: Channel::from_version(&ver).unwrap_or(Channel::Nightly),
                            version: vp.install.as_ref().map_or_else(
                                || vp.package.as_ref().unwrap().version.clone(),
                                |v| v.version.clone(),
                            ),
                            installed: vp.install.is_some(),
                            debug_info: DebugInfo {
                                install: vp.install.map(DebugInstall::from),
                                package: vp.package,
                            },
                        })
                        .collect::<Vec<_>>()
                )?
            );
        } else {
            print_table(version_set.into_values().map(|vp| match vp.install {
                Some(v) => (v.version, true),
                None => (vp.package.unwrap().version, false),
            }));
        }
    }
    Ok(())
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    #[arg(long)]
    pub installed_only: bool,

    /// Single column output.
    #[arg(long, value_parser=[
        "major-version", "installed", "available",
    ])]
    pub column: Option<String>,

    /// Output in JSON format.
    #[arg(long)]
    pub json: bool,
}

#[derive(serde::Serialize)]
pub struct DebugInstall {
    path: Option<PathBuf>,
    server_path: Option<PathBuf>,
    #[serde(flatten)]
    install: InstallInfo,
}

#[derive(serde::Serialize)]
pub struct DebugInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<PackageInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    install: Option<DebugInstall>,
}

pub struct Pair {
    package: Option<PackageInfo>,
    install: Option<InstallInfo>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct JsonVersionInfo {
    channel: Channel,
    version: ver::Build,
    installed: bool,
    debug_info: DebugInfo,
}

pub fn all_packages() -> Vec<PackageInfo> {
    let mut pkgs = Vec::with_capacity(16);
    match get_server_packages(Channel::Stable) {
        Ok(stable) => pkgs.extend(stable),
        Err(e) => log::warn!("Unable to fetch stable packages: {:#}", e),
    };
    match get_server_packages(Channel::Testing) {
        Ok(testing) => pkgs.extend(testing),
        Err(e) => log::warn!("Unable to fetch testing packages: {:#}", e),
    };
    match get_server_packages(Channel::Nightly) {
        Ok(nightly) => pkgs.extend(nightly),
        Err(e) => log::warn!("Unable to fetch nightly packages: {:#}", e),
    }
    pkgs
}

fn print_table(items: impl Iterator<Item = (ver::Build, bool)>) {
    let mut table = Table::new();
    table.set_format(*table::FORMAT);
    table.add_row(Row::new(vec![
        table::header_cell("Channel"),
        table::header_cell("Version"),
        table::header_cell("Installed"),
    ]));
    for (ver, installed) in items {
        let channel = Channel::from_version(&ver.specific());
        table.add_row(Row::new(vec![
            Cell::new(channel.as_ref().map_or("nightly", |x| x.as_str())),
            Cell::new(&ver.to_string()),
            Cell::new(if installed { "✓" } else { "" }),
        ]));
    }
    table.printstd();
}

impl DebugInstall {
    fn from(install: InstallInfo) -> DebugInstall {
        DebugInstall {
            path: install.base_path().ok(),
            server_path: install.server_path().ok(),
            install,
        }
    }
}
