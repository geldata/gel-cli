use std::collections::BTreeMap;
use std::path::PathBuf;

use color_print::cprintln;
use gel_cli_derive::IntoArgs;

use crate::branding::BRANDING;
use crate::portable::local::{self, InstallInfo};
use crate::portable::repository::{Channel, PackageInfo, get_server_packages};
use crate::portable::ver::{self};
use crate::table::{self, Cell, Row, Table};

pub fn run(cmd: &Command) -> Result<(), anyhow::Error> {
    let mut channel = cmd.channel;
    if channel.is_none() && !cmd.installed_only {
        channel = Some(Channel::Stable);
    }

    let no_opts = cmd.version.is_none() && cmd.channel.is_none() && !cmd.installed_only && !cmd.all;

    // Combine the channel, version and installed_only into a single string of plain english:
    if no_opts {
        cprintln!("Listing <bold>installed and stable</bold> versions of <bold>{BRANDING}</bold>:");
    } else if cmd.all {
        cprintln!(
            "Listing <bold>all available</bold> versions and channels of <bold>{BRANDING}</bold>:"
        );
    } else {
        let filter = if cmd.installed_only {
            let mut filter = "installed".to_string();
            if cmd.version.is_some() {
                filter.push_str(", matching");
            }
            if let Some(channel) = channel {
                filter.push_str(&format!(", {channel:?} channel").to_lowercase());
            }
            filter
        } else {
            let mut filter = if cmd.version.is_some() {
                "matching, "
            } else if channel == Some(Channel::Stable) {
                "current, "
            } else {
                ""
            }
            .to_string();
            if let Some(channel) = channel {
                filter.push_str(&format!("{channel:?} channel").to_lowercase());
            }
            filter
        };

        cprintln!("Listing <bold>{filter}</bold> versions of <bold>{BRANDING}</bold>:");
    }

    let installed = local::get_installed()?;
    let installed_count = installed.len();
    let all_packages = all_packages();
    let all_package_count = all_packages.len();

    // Determine the latest stable version
    let latest_stable = all_packages
        .iter()
        .filter(|p| {
            let channel = Channel::from_version(&p.version.specific()).unwrap_or(Channel::Nightly);
            channel == Channel::Stable
        })
        .max_by_key(|p| p.version.specific())
        .map(|p| p.version.specific())
        .expect("No stable version found");

    let mut version = cmd.version.clone();
    if version.is_none() && !cmd.installed_only {
        version = Some(ver::Filter {
            major: latest_stable.major,
            minor: None,
            exact: false,
        })
    }

    log::debug!(
        "channel: {:?}, version: {:?}, installed_only: {:?}",
        channel,
        version,
        cmd.installed_only
    );

    let mut packages = if cmd.installed_only {
        installed
            .into_iter()
            .map(|v| JsonVersionInfo {
                channel: Channel::from_version(&v.version.specific()).unwrap_or(Channel::Nightly),
                version: v.version.clone(),
                installed: true,
                debug_info: DebugInfo {
                    package: None,
                    install: Some(DebugInstall::from(v)),
                },
            })
            .collect::<Vec<_>>()
    } else {
        let mut installed = installed
            .into_iter()
            .map(|v| (v.version.specific(), v))
            .collect::<BTreeMap<_, _>>();
        all_packages
            .into_iter()
            .map(|p| {
                let installed = installed.remove(&p.version.specific());
                JsonVersionInfo {
                    channel: Channel::from_version(&p.version.specific())
                        .unwrap_or(Channel::Nightly),
                    version: p.version.clone(),
                    installed: installed.is_some(),
                    debug_info: DebugInfo {
                        package: Some(p),
                        install: installed.map(DebugInstall::from),
                    },
                }
            })
            .collect::<Vec<_>>()
    };

    if !cmd.all {
        packages = packages
            .into_iter()
            .filter(|p| {
                // DX: This is most useful for users: if no options, union the
                // stable/current-version results with the installed versions.
                if no_opts && p.installed {
                    return true;
                }
                if let Some(channel) = channel {
                    let package_channel =
                        Channel::from_version(&p.version.specific()).unwrap_or(Channel::Nightly);
                    if package_channel != channel {
                        return false;
                    }
                }
                if let Some(version) = &version {
                    if channel == Some(Channel::Stable) || cmd.version.is_some() {
                        version.matches_loose(&p.version.specific())
                    } else {
                        // Dx: For testing/nightly without a specific version,
                        // show the next version as well.
                        version.matches_loose(&p.version.specific())
                            || ver::Filter {
                                major: version.major + 1,
                                minor: None,
                                exact: false,
                            }
                            .matches_loose(&p.version.specific())
                    }
                } else {
                    true
                }
            })
            .collect::<Vec<_>>();
    }
    packages.sort_by(|a, b| a.version.specific().cmp(&b.version.specific()));

    if cmd.json {
        print!("{}", serde_json::to_string_pretty(&packages)?);
    } else {
        let count = packages.len();
        let count_installed = packages.iter().filter(|p| p.installed).count();
        print_table(packages.into_iter().map(|p| (p.version, p.installed)));

        if no_opts {
            let hidden = all_package_count - count;
            cprintln!(
                "HINT: {hidden} versions were hidden. To list all available versions, use the <bold>--all</bold> flag."
            );
        }
        if count_installed < installed_count {
            let hidden = installed_count - count_installed;
            cprintln!(
                "HINT: {hidden} installed versions were hidden. To list all installed versions, use the <bold>--installed-only</bold> flag."
            );
        }
    }
    Ok(())
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    #[arg(long, conflicts_with_all=&["all"])]
    pub installed_only: bool,

    /// Show all versions including older and non-stable channels.
    #[arg(long)]
    pub all: bool,

    /// Show only versions for the specified channel.
    #[arg(long, conflicts_with_all=&["all"])]
    #[arg(value_enum)]
    pub channel: Option<Channel>,

    /// Show only versions matching the specified version.
    #[arg(long, conflicts_with_all=&["all"])]
    pub version: Option<ver::Filter>,

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

#[derive(serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct JsonVersionInfo {
    channel: Channel,
    version: ver::Build,
    installed: bool,
    debug_info: DebugInfo,
}

#[tokio::main(flavor = "current_thread")]
pub async fn all_packages() -> Vec<PackageInfo> {
    let mut pkgs = Vec::with_capacity(16);
    match get_server_packages(Channel::Stable).await {
        Ok(stable) => pkgs.extend(stable),
        Err(e) => log::warn!("Unable to fetch stable packages: {e:#}"),
    };
    match get_server_packages(Channel::Testing).await {
        Ok(testing) => pkgs.extend(testing),
        Err(e) => log::warn!("Unable to fetch testing packages: {e:#}"),
    };
    match get_server_packages(Channel::Nightly).await {
        Ok(nightly) => pkgs.extend(nightly),
        Err(e) => log::warn!("Unable to fetch nightly packages: {e:#}"),
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
