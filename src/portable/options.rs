use std::fmt;
use std::str::FromStr;

use clap::ValueHint;
use serde::{Serialize, Deserialize};
use edgedb_cli_derive::IntoArgs;

use crate::commands::ExitCode;
use crate::portable::local::{is_valid_local_instance_name, is_valid_cloud_name};
use crate::portable::ver;
use crate::portable::repository::Channel;
use crate::print::{echo, warn, err_marker};
use crate::process::{self, IntoArg};
use crate::options::{ConnectionOptions, CloudOptions};
use crate::cloud::ops::CloudTier;


const DOMAIN_LABEL_MAX_LENGTH: usize = 63;
const CLOUD_INSTANCE_NAME_MAX_LENGTH: usize = DOMAIN_LABEL_MAX_LENGTH - 2 + 1;  // "--" -> "/"

#[derive(clap::Args, Debug, Clone)]
pub struct ServerCommand {
    #[command(subcommand)]
    pub subcommand: Command,
}

#[derive(clap::Args, Debug, Clone)]
#[command(version = "help_expand")]
#[command(disable_version_flag=true)]
pub struct ServerInstanceCommand {
    #[command(subcommand)]
    pub subcommand: InstanceCommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum InstanceCommand {
    /// Initialize a new EdgeDB instance.
    Create(Create),
    /// Show all instances.
    List(List),
    /// Show status of an instance.
    Status(Status),
    /// Start an instance.
    Start(Start),
    /// Stop an instance.
    Stop(Stop),
    /// Restart an instance.
    Restart(Restart),
    /// Destroy an instance and remove the data.
    Destroy(Destroy),
    /// Link a remote instance.
    Link(Link),
    /// Unlink a remote instance.
    Unlink(Unlink),
    /// Show logs for an instance.
    Logs(Logs),
    /// Resize a Cloud instance.
    Resize(Resize),
    /// Upgrade installations and instances.
    Upgrade(Upgrade),
    /// Revert a major instance upgrade.
    Revert(Revert),
    /// Generate new password for instance user (randomly generated by default).
    ResetPassword(ResetPassword),
    /// Display instance credentials (add `--json` for verbose).
    Credentials(ShowCredentials),
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum Command {
    /// Show locally installed EdgeDB versions.
    Info(Info),
    /// Install an EdgeDB version locally.
    Install(Install),
    /// Uninstall an EdgeDB version locally.
    Uninstall(Uninstall),
    /// List available and installed versions of EdgeDB.
    ListVersions(ListVersions),
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Install {
    #[arg(short='i', long)]
    pub interactive: bool,
    #[arg(long, conflicts_with_all=&["channel", "version"])]
    pub nightly: bool,
    #[arg(long, conflicts_with_all=&["nightly", "channel"])]
    pub version: Option<ver::Filter>,
    #[arg(long, conflicts_with_all=&["nightly", "version"])]
    #[arg(value_enum)]
    pub channel: Option<Channel>,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Uninstall {
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
    #[arg(short='v', long)]
    pub verbose: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct ListVersions {
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[derive(clap::ValueEnum)]
#[value(rename_all="kebab-case")]
pub enum StartConf {
    Auto,
    Manual,
}

#[derive(Clone, Debug)]
pub enum InstanceName {
    Local(String),
    Cloud {
        org_slug: String,
        name: String,
    },
}

fn billable_unit(s: &str) -> Result<String, String> {
    let (numerator, denominator) = match s.split_once('/') {
        Some(v) => v,
        None => (s, "1"),
    };

    let n: u64 = numerator
        .parse()
        .map_err(|_| format!("`{s}` is not a positive number or valid fraction"))?;

    let d: u64 = denominator
        .parse()
        .map_err(|_| format!("`{s}` is not a positive number or valid fraction"))?;

    if n == 0 || d == 0 {
        Err(String::from("`{s}` is not a positive number or valid fraction"))
    } else {
        Ok(s.to_string())
    }
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct CloudInstanceBillables {
    /// Cloud instance subscription tier.
    #[arg(long, value_name="tier")]
    #[arg(value_enum)]
    pub tier: Option<CloudTier>,

    /// The size of compute to be allocated for the Cloud instance in
    /// Compute Units.
    #[arg(long, value_name="number", value_parser=billable_unit)]
    pub compute_size: Option<String>,

    /// The size of storage to be allocated for the Cloud instance in
    /// Gigabytes.
    #[arg(long, value_name="GiB", value_parser=billable_unit)]
    pub storage_size: Option<String>,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct CloudInstanceParams {
    /// The region in which to create the instance (for cloud instances).
    #[arg(long)]
    pub region: Option<String>,

    #[command(flatten)]
    pub billables: CloudInstanceBillables,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct CloudBackupSourceParams {
    // The name of the instance that should be used as the source
    // of the backup.
    #[arg(long)]
    pub from_instance: Option<InstanceName>,

    // The ID of the backup to restore from.
    #[arg(long)]
    pub from_backup_id: Option<String>,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Create {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Name of instance to create. Asked interactively if not specified.
    #[arg(value_hint=ValueHint::Other)]
    pub name: Option<InstanceName>,

    /// Create instance under latest nightly version.
    #[arg(long, conflicts_with_all=&["channel", "version"])]
    pub nightly: bool,
    /// Create instance under latest nightly version.
    #[arg(long, conflicts_with_all=&["nightly", "channel"])]
    /// Create instance with specified version.
    pub version: Option<ver::Filter>,
    /// Indicate channel (stable, testing, or nightly) for instance to create.
    #[arg(long, conflicts_with_all=&["nightly", "version"])]
    #[arg(value_enum)]
    pub channel: Option<Channel>,
    /// Indicate port for instance to create.
    #[arg(long)]
    pub port: Option<u16>,

    #[command(flatten)]
    pub cloud_params: CloudInstanceParams,

    #[command(flatten)]
    pub cloud_backup_source: CloudBackupSourceParams,

    /// Deprecated parameter, unused.
    #[arg(long, hide=true)]
    pub start_conf: Option<StartConf>,

    /// Default user name (created during initialization and saved in
    /// credentials file).
    #[arg(long, default_value="edgedb")]
    pub default_user: String,

    /// The default branch name. This defaults to 'main' on EdgeDB >=5.x; otherwise
    /// 'edgedb' is used.
    pub default_branch: Option<String>,

    /// Do not ask questions. Assume user wants to upgrade instance.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Destroy {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Name of instance to destroy.
    #[arg(hide=true)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub name: Option<InstanceName>,

    /// Name of instance to destroy.
    #[arg(short='I', long)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: Option<InstanceName>,

    /// Verbose output.
    #[arg(short='v', long, overrides_with="quiet")]
    pub verbose: bool,

    /// Quiet output.
    #[arg(short='q', long, overrides_with="verbose")]
    pub quiet: bool,

    /// Force destroy even if instance is referred to by a project.
    #[arg(long)]
    pub force: bool,

    /// Do not ask questions. Assume user wants to delete instance.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(clap::Args, Clone, Debug)]
#[command(long_about = "Link to a remote EdgeDB instance and
assign an instance name to simplify future connections.")]
pub struct Link {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Specify a new instance name for the remote server. User will
    /// be prompted to provide a name if not specified.
    #[arg(value_hint=ValueHint::Other)]
    pub name: Option<InstanceName>,

    /// Run in non-interactive mode (accepting all defaults).
    #[arg(long)]
    pub non_interactive: bool,

    /// Reduce command verbosity.
    #[arg(long)]
    pub quiet: bool,

    /// Trust peer certificate.
    #[arg(long)]
    pub trust_tls_cert: bool,

    /// Overwrite existing credential file if any.
    #[arg(long)]
    pub overwrite: bool,
}

#[derive(clap::Args, Clone, Debug)]
#[command(long_about = "Unlink from a remote EdgeDB instance.")]
pub struct Unlink {
    /// Specify remote instance name.
    #[arg(hide=true)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub name: Option<InstanceName>,

    /// Specify remote instance name.
    #[arg(short='I', long)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: Option<InstanceName>,

    /// Force destroy even if instance is referred to by a project.
    #[arg(long)]
    pub force: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Start {
    /// Name of instance to start.
    #[arg(hide=true)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub name: Option<InstanceName>,

    /// Name of instance to start.
    #[arg(short='I', long)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: Option<InstanceName>,

    /// Start server in the foreground.
    #[arg(long)]
    #[cfg_attr(target_os="linux",
        arg(help="Start the server in the foreground rather than using \
                  systemd to manage the process (note: you might need to \
                  stop the non-foreground instance first)"))]
    #[cfg_attr(target_os="macos",
        arg(help="Start the server in the foreground rather than using \
                  launchctl to manage the process (note: you might need to \
                  stop the non-foreground instance first)"))]
    pub foreground: bool,

    /// With `--foreground`, stops server running in the background; also restarts
    /// the service on exit.
    #[arg(long, conflicts_with="managed_by")]
    pub auto_restart: bool,

    /// Indicate whether managed by edgedb-cli, systemd, launchctl, or None.
    #[arg(long, hide=true)]
    #[arg(value_parser=["systemd", "launchctl", "edgedb-cli"])]
    #[arg(conflicts_with="auto_restart")]
    pub managed_by: Option<String>,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Stop {
    /// Name of instance to stop.
    #[arg(hide=true)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub name: Option<InstanceName>,

    /// Name of instance to stop.
    #[arg(short='I', long)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: Option<InstanceName>,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Restart {
    /// Name of instance to restart.
    #[arg(hide=true)]
    #[arg(value_hint=ValueHint::Other)]
    pub name: Option<InstanceName>,

    /// Name of instance to restart.
    #[arg(short='I', long)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: Option<InstanceName>,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct List {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Output more debug info about each instance.
    #[arg(long, conflicts_with_all=&["debug", "json"])]
    pub extended: bool,

    /// Output all available debug info about each instance.
    #[arg(long, hide=true)]
    #[arg(conflicts_with_all=&["extended", "json"])]
    pub debug: bool,

    /// Output in JSON format.
    #[arg(long, conflicts_with_all=&["extended", "debug"])]
    pub json: bool,

    /// Query remote instances.
    //  Currently needed for WSL.
    #[arg(long, hide=true)]
    pub no_remote: bool,

    /// Do not show warnings on no instances.
    //  Currently needed for WSL.
    #[arg(long, hide=true)]
    pub quiet: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Status {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Name of instance.
    #[arg(hide=true)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub name: Option<InstanceName>,

    /// Name of instance.
    #[arg(short='I', long)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: Option<InstanceName>,

    /// Show current systems service info.
    #[arg(long, conflicts_with_all=&["debug", "json", "extended"])]
    pub service: bool,

    /// Output more debug info about each instance.
    #[arg(long, conflicts_with_all=&["debug", "json", "service"])]
    pub extended: bool,

    /// Output all available debug info about each instance.
    #[arg(long, hide=true)]
    #[arg(conflicts_with_all=&["extended", "json", "service"])]
    pub debug: bool,

    /// Output in JSON format.
    #[arg(long, conflicts_with_all=&["extended", "debug", "service"])]
    pub json: bool,

    /// Do not print error on "No instance found", only indicate by error code.
    //  Currently needed for WSL.
    #[arg(long, hide=true)]
    pub quiet: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Logs {
    /// Name of instance.
    #[arg(hide=true)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub name: Option<InstanceName>,

    /// Name of instance.
    #[arg(short='I', long)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: Option<InstanceName>,

    /// Number of lines to show.
    #[arg(short='n', long)]
    pub tail: Option<usize>,

    /// Show log tail and continue watching for new entries.
    #[arg(short='f', long)]
    pub follow: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Resize {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Instance to resize.
    #[arg(short='I', long, required=true)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: InstanceName,

    #[command(flatten)]
    pub billables: CloudInstanceBillables,

    /// Do not ask questions.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Upgrade {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Upgrade specified instance to latest /version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_testing", "to_nightly", "to_channel",
    ])]
    pub to_latest: bool,

    /// Upgrade specified instance to a specified version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_testing", "to_latest", "to_nightly", "to_channel",
    ])]
    pub to_version: Option<ver::Filter>,

    /// Upgrade specified instance to latest nightly version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_latest", "to_testing", "to_channel",
    ])]
    pub to_nightly: bool,

    /// Upgrade specified instance to latest testing version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_latest", "to_nightly", "to_channel",
    ])]
    pub to_testing: bool,

    /// Upgrade specified instance to latest version in the channel.
    #[arg(long, value_enum)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_latest", "to_nightly", "to_testing",
    ])]
    pub to_channel: Option<Channel>,

    /// Instance to upgrade.
    #[arg(hide=true)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub name: Option<InstanceName>,

    /// Instance to upgrade.
    #[arg(short='I', long)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: Option<InstanceName>,

    /// Verbose output.
    #[arg(short='v', long)]
    pub verbose: bool,

    /// Force upgrade even if there is no new version.
    #[arg(long)]
    pub force: bool,

    /// Force dump-restore during upgrade even if version is compatible.
    ///
    /// Used by `project upgrade --force`.
    #[arg(long, hide=true)]
    pub force_dump_restore: bool,

    /// Do not ask questions. Assume user wants to upgrade instance.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Revert {
    /// Name of instance to revert.
    #[arg(hide=true)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub name: Option<InstanceName>,

    /// Name of instance to revert.
    #[arg(short='I', long)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: Option<InstanceName>,

    /// Do not check if upgrade is in progress.
    #[arg(long)]
    pub ignore_pid_check: bool,

    /// Do not ask for confirmation.
    #[arg(short='y', long)]
    pub no_confirm: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct ResetPassword {
    /// Name of instance to reset.
    #[arg(hide=true)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub name: Option<InstanceName>,

    /// Name of instance to reset.
    #[arg(short='I', long)]
    #[arg(value_hint=ValueHint::Other)]  // TODO complete instance name
    pub instance: Option<InstanceName>,

    /// User to change password for (default obtained from credentials file).
    #[arg(long)]
    pub user: Option<String>,
    /// Read password from the terminal rather than generating a new one.
    #[arg(long)]
    pub password: bool,
    /// Read password from stdin rather than generating a new one.
    #[arg(long)]
    pub password_from_stdin: bool,
    /// Save new user and password into a credentials file. By default
    /// credentials file is updated only if user name matches.
    #[arg(long)]
    pub save_credentials: bool,
    /// Do not save generated password into a credentials file even if user name matches.
    #[arg(long)]
    pub no_save_credentials: bool,
    /// Do not print any messages, only indicate success by exit status.
    #[arg(long)]
    pub quiet: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Info {
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

    #[arg(long, value_parser=["bin-path", "version"])]
    /// Get specific value:
    ///
    /// * `bin-path` -- Path to the server binary
    /// * `version` -- Server version
    pub get: Option<String>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ShowCredentials {
    #[command(flatten)]
    pub cloud_opts: ConnectionOptions,

    /// Output in JSON format (password is included in cleartext).
    #[arg(long)]
    pub json: bool,
    /// Output a DSN with password in cleartext.
    #[arg(long)]
    pub insecure_dsn: bool,
}

impl FromStr for StartConf {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<StartConf> {
        match s {
            "auto" => Ok(StartConf::Auto),
            "manual" => Ok(StartConf::Manual),
            _ => anyhow::bail!("Unsupported start configuration, \
                options: `auto`, `manual`"),
        }
    }
}

impl IntoArg for &StartConf {
    fn add_arg(self, process: &mut process::Native) {
        process.arg(self.as_str());
    }
}

impl StartConf {
    pub fn as_str(&self) -> &str {
        match self {
            StartConf::Auto => "auto",
            StartConf::Manual => "manual",
        }
    }
}

impl fmt::Display for StartConf {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

impl fmt::Display for InstanceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstanceName::Local(name) => name.fmt(f),
            InstanceName::Cloud { org_slug, name } => write!(f, "{}/{}", org_slug, name),
        }
    }
}

impl FromStr for InstanceName {
    type Err = anyhow::Error;
    fn from_str(name: &str) -> anyhow::Result<InstanceName> {
        if let Some((org_slug, instance_name)) = name.split_once('/') {
            if !is_valid_cloud_name(instance_name) {
                anyhow::bail!(
                    "instance name \"{}\" must be a valid identifier, \
                     regex: ^[a-zA-Z0-9]+(-[a-zA-Z0-9]+)*$",
                    instance_name,
                );
            }
            if !is_valid_cloud_name(org_slug) {
                anyhow::bail!(
                    "org name \"{}\" must be a valid identifier, \
                     regex: ^[a-zA-Z0-9]+(-[a-zA-Z0-9]+)*$",
                    org_slug,
                );
            }
            if name.len() > CLOUD_INSTANCE_NAME_MAX_LENGTH {
                anyhow::bail!(
                    "invalid cloud instance name \"{}\": \
                    length cannot exceed {} characters",
                    name, CLOUD_INSTANCE_NAME_MAX_LENGTH,
                );
            }
            Ok(InstanceName::Cloud {
                org_slug: org_slug.into(),
                name: instance_name.into(),
            })
        } else {
            if !is_valid_local_instance_name(name) {
                anyhow::bail!(
                    "instance name must be a valid identifier, \
                     regex: ^[a-zA-Z_0-9]+(-[a-zA-Z_0-9]+)*$ or \
                     a cloud instance name ORG/INST."
                );
            }
            Ok(InstanceName::Local(name.into()))
        }
    }
}

impl IntoArg for &InstanceName {
    fn add_arg(self, process: &mut process::Native) {
        process.arg(self.to_string());
    }
}

pub fn instance_arg<'x>(positional: &'x Option<InstanceName>,
                        named: &'x Option<InstanceName>)
                        -> anyhow::Result<&'x InstanceName>
{
    if let Some(name) = positional {
        if named.is_some() {
            echo!(err_marker(), "Instance name is specified twice \
                as positional argument and via `-I`. \
                The latter is preferred.");
            return Err(ExitCode::new(2).into());
        }
        warn(format_args!("Specifying instance name as positional argument is \
            deprecated. Use `-I {}` instead.", name));
        return Ok(name);
    }
    if let Some(name) = named {
        return Ok(name);
    }
    echo!(err_marker(), "Instance name argument is required, use '-I name'");
    Err(ExitCode::new(2).into())
}
