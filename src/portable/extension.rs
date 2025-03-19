use std::ffi::OsStr;
use std::path::Path;

use anyhow::Context;
use edgedb_cli_derive::IntoArgs;
use log::{debug, trace};
use prettytable::{Table, row};

use crate::branding::{BRANDING_CLI_CMD, BRANDING_CLOUD};
use crate::hint::HintExt;
use crate::options::{InstanceOptions, InstanceOptionsGlobal, Options};
use crate::portable::local::InstanceInfo;
use crate::portable::options::InstanceName;
use crate::portable::platform::get_server;
use crate::portable::repository::{Channel, get_platform_extension_packages};
use crate::portable::server::install::download_package;
use crate::portable::windows;
use crate::print::Highlight;
use crate::{print, table};

pub fn run(cmd: &Command, options: &Options) -> Result<(), anyhow::Error> {
    use Subcommands::*;
    match &cmd.subcommand {
        Install(c) => install(c, options),
        List(c) => list(c, options),
        ListAvailable(c) => list_available(c, options),
        Uninstall(c) => uninstall(c, options),
    }
}

#[derive(clap::Args, Debug, Clone)]
#[command(version = "help_expand")]
#[command(disable_version_flag = true)]
pub struct Command {
    #[command(subcommand)]
    pub subcommand: Subcommands,

    #[command(flatten)]
    pub instance_opts: InstanceOptionsGlobal,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum Subcommands {
    /// List installed extensions
    List(ExtensionList),
    /// List available extensions
    ListAvailable(ExtensionListAvailable),
    /// Install an extension
    Install(ExtensionInstall),
    /// Uninstall an extension
    Uninstall(ExtensionUninstall),
}

#[derive(clap::Args, Debug, Clone)]
pub struct ExtensionList {
    #[command(flatten)]
    pub instance_opts: InstanceOptions,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct ExtensionListAvailable {
    /// Specify the channel override (stable, testing, or nightly)
    #[arg(long, hide = true)]
    pub channel: Option<Channel>,
    /// Specify the slot override (for development use)
    #[arg(long, hide = true)]
    pub slot: Option<String>,

    #[command(flatten)]
    pub instance_opts: InstanceOptions,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct ExtensionInstall {
    #[command(flatten)]
    pub instance_opts: InstanceOptions,

    /// Name of the extension to install
    pub extension: String,
    /// Specify the channel override (stable, testing, or nightly)
    #[arg(long, hide = true)]
    pub channel: Option<Channel>,
    /// Specify the slot override (for development use)
    #[arg(long, hide = true)]
    pub slot: Option<String>,
    /// Reinstall the extension if it's already installed
    #[arg(long, hide = true)]
    pub reinstall: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct ExtensionUninstall {
    #[command(flatten)]
    pub instance_opts: InstanceOptions,

    /// Name of the extension to uninstall
    pub extension: String,
}

fn get_local_instance(instance: InstanceName) -> Result<InstanceInfo, anyhow::Error> {
    let name = match instance {
        InstanceName::Local(name) => name,
        inst_name => {
            return Err(anyhow::anyhow!(
                "cannot install extensions in {BRANDING_CLOUD} instance {}.",
                inst_name
            ))
            .with_hint(|| {
                format!("only local instances can install extensions ({inst_name} is remote)")
            })?;
        }
    };
    let Some(inst) = InstanceInfo::try_read(&name)? else {
        return Err(anyhow::anyhow!(
            "cannot install extensions in {BRANDING_CLOUD} instance {}.",
            name
        ))
        .with_hint(|| format!("only local instances can install extensions ({name} is remote)"))?;
    };
    Ok(inst)
}

type ExtensionInfo = (String, String);

fn get_extensions(options: &Options) -> Result<Vec<ExtensionInfo>, anyhow::Error> {
    // if remote or cloud instance, connect and query extension packages
    let connector = options.block_on_create_connector()?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let extensions = connector.run_single_query::<ExtensionInfo>(
        "for ext in sys::ExtensionPackage union (
            with
                ver := ext.version,
                ver_str := <str>ver.major++'.'++<str>ver.minor,
            select (ext.name, ver_str)
        );",
    );

    rt.block_on(extensions)
}

fn list(_: &ExtensionList, options: &Options) -> Result<(), anyhow::Error> {
    let extensions = get_extensions(options)?;

    let mut table = Table::new();
    table.set_format(*table::FORMAT);
    table.set_titles(row!["Name", "Version"]);
    for (name, version) in extensions {
        table.add_row(row![name, version]);
    }
    table.printstd();

    Ok(())
}

fn uninstall(cmd: &ExtensionUninstall, _options: &Options) -> Result<(), anyhow::Error> {
    let inst = get_local_instance(cmd.instance_opts.instance()?)?;

    if cfg!(windows) {
        return windows::extension_uninstall(cmd);
    }

    run_extension_loader(
        &inst,
        Some("--uninstall".to_string()),
        Some(Path::new(&cmd.extension)),
    )?;
    Ok(())
}

fn install(cmd: &ExtensionInstall, _options: &Options) -> Result<(), anyhow::Error> {
    let inst = get_local_instance(cmd.instance_opts.instance()?)?;

    if cfg!(windows) {
        return windows::extension_install(cmd);
    }

    let version = inst.get_version()?.specific();
    let channel = cmd.channel.unwrap_or(Channel::Stable);
    let slot = cmd.slot.clone().unwrap_or(version.extension_server_slot());
    debug!("Instance: {version} {channel:?} {slot}");
    let packages = get_platform_extension_packages(channel, &slot, get_server()?)?;

    let package = packages
        .iter()
        .find(|pkg| pkg.tags.get("extension").cloned().unwrap_or_default() == cmd.extension);

    match package {
        Some(pkg) => {
            print::msg!(
                "Found extension package: {} version {}",
                cmd.extension,
                pkg.version.to_string().emphasized()
            );
            let zip = download_package(pkg)?;
            let command = if cmd.reinstall {
                Some("--reinstall")
            } else {
                None
            };
            run_extension_loader(&inst, command, Some(&zip))?;
            print::msg!(
                "Extension '{}' installed successfully.",
                cmd.extension.as_str().emphasized().success()
            );
            print::msg!(
                "{}",
                "Hint: before using the extension, the instance has to be restarted:".muted()
            );
            print::msg!(
                "{}",
                format!("  {BRANDING_CLI_CMD} instance restart -I {}", inst.name).muted()
            );
        }
        None => {
            return Err(anyhow::anyhow!(
                "Extension '{}' not found in available packages.",
                cmd.extension
            ));
        }
    }

    Ok(())
}

fn run_extension_loader(
    instance: &InstanceInfo,
    command: Option<impl AsRef<OsStr>>,
    file: Option<impl AsRef<OsStr>>,
) -> Result<String, anyhow::Error> {
    let ext_path = instance.extension_loader_path()?;

    let mut cmd = std::process::Command::new(&ext_path);

    if let Some(cmd_str) = command {
        cmd.arg(cmd_str);
    }

    if let Some(file_path) = file {
        cmd.arg(file_path);
    }

    let output = cmd
        .output()
        .with_context(|| format!("Failed to execute {}", ext_path.display()))?;

    if !output.status.success() {
        eprintln!("STDOUT:\n{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("STDERR:\n{}", String::from_utf8_lossy(&output.stderr));
        return Err(anyhow::anyhow!(
            "Extension installation failed with exit code: {}",
            output.status
        ));
    } else {
        trace!("STDOUT:\n{}", String::from_utf8_lossy(&output.stdout));
        trace!("STDERR:\n{}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn list_available(list: &ExtensionListAvailable, _options: &Options) -> Result<(), anyhow::Error> {
    let inst = get_local_instance(list.instance_opts.instance()?)?;

    let version = inst.get_version()?.specific();
    let channel = list.channel.unwrap_or(Channel::Stable);
    let slot = list.slot.clone().unwrap_or(version.extension_server_slot());
    debug!("Instance: {version} {channel:?} {slot}");
    let packages = get_platform_extension_packages(channel, &slot, get_server()?)?;

    let mut table = Table::new();
    table.set_format(*table::FORMAT);
    table.set_titles(row!["Name", "Version"]);
    for pkg in packages {
        let ext = pkg.tags.get("extension").cloned().unwrap_or_default();
        table.add_row(row![ext, pkg.version]);
    }
    table.printstd();
    Ok(())
}
