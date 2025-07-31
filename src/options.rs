use std::env;
use std::io::stdin;
use std::path::PathBuf;
use std::time::Duration;

use color_print::cformat;
use const_format::concatcp;
use gel_errors::{ClientNoCredentialsError, NoCloudConfigFound};
use gel_protocol::model;
use gel_tokio::dsn::{TlsSecurity, UnixPath};
use gel_tokio::{Builder, InstanceName};
use log::warn;
use std::io::IsTerminal;
use tokio::task::spawn_blocking as unblock;

use gel_cli_derive::IntoArgs;

use crate::docker::{DockerMode, has_docker_blocking, try_docker, try_docker_fallback};
use crate::{cli, msg, watch};

use crate::branding::{BRANDING, BRANDING_CLI_CMD, BRANDING_CLOUD, MANIFEST_FILE_DISPLAY_NAME};
use crate::cloud::options::CloudCommand;
use crate::commands::ExitCode;
use crate::commands::parser::Common;
use crate::connect::Connector;
use crate::hint::HintExt;
use crate::markdown;
use crate::portable;
use crate::portable::local::runstate_dir;
use crate::print::{self, AsRelativeToCurrentDir, Highlight, err_marker};
use crate::project;
use crate::repl::{InputLanguage, OutputFormat};
use crate::tty_password;

const MAX_TERM_WIDTH: usize = 100;
const MIN_TERM_WIDTH: usize = 50;

const CONN_OPTIONS_GROUP: &str = concatcp!(
    "Connection Options (",
    BRANDING_CLI_CMD,
    " --help-connect to see full list)"
);
const CLOUD_OPTIONS_GROUP: &str = concatcp!(BRANDING_CLOUD, " Connection Options");
const CONNECTION_ARG_HINT: &str = concatcp!(
    "\
    Run `",
    BRANDING_CLI_CMD,
    " project init` or use any of `-H`, `-P`, `-I` arguments \
    to specify connection parameters. See `--help` for details"
);

#[derive(clap::Args, Clone, Debug, Default)]
#[group(id = "connopts")]
pub struct ConnectionOptions {
    #[command(flatten)]
    pub instance_opts: InstanceOptionsGlobal,

    /// DSN for [`BRANDING`] to connect to (overrides all other options
    /// except password)
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(conflicts_with_all=&["instance"])]
    #[arg(global = true)]
    pub dsn: Option<String>,

    /// Path to JSON file to read credentials from
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(conflicts_with_all=&["dsn", "instance"])]
    #[arg(hide = true)]
    #[arg(global = true)]
    pub credentials_file: Option<PathBuf>,

    /// Gel instance host
    #[arg(short='H', long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(value_hint=clap::ValueHint::Hostname)]
    #[arg(hide = true)]
    #[arg(global = true)]
    #[arg(conflicts_with_all=
          &["dsn", "credentials_file", "instance", "unix_path"])]
    pub host: Option<String>,

    /// Port to connect to Gel
    #[arg(short='P', long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(hide = true)]
    #[arg(global = true)]
    #[arg(conflicts_with_all=&["dsn", "credentials_file", "instance"])]
    pub port: Option<u16>,

    /// A path to a Unix socket for Gel connection
    ///
    /// When the supplied path is a directory, the actual path will be
    /// computed using the `--port` and `--admin` parameters.
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(value_hint=clap::ValueHint::AnyPath)]
    #[arg(hide = true)]
    #[arg(global = true)]
    #[arg(conflicts_with_all=
          &["dsn", "credentials_file", "instance", "host"])]
    pub unix_path: Option<PathBuf>,

    /// Gel user name
    #[arg(short='u', long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(hide = true)]
    #[arg(global = true)]
    pub user: Option<String>,

    /// Database name to connect to
    #[arg(short='d', long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(value_hint=clap::ValueHint::Other)] // TODO auto-complete for database
    #[arg(hide = true)]
    #[arg(global = true)]
    pub database: Option<String>,

    /// Branch to connect with
    #[arg(short='b', long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(value_hint=clap::ValueHint::Other)] // TODO auto-complete for branch
    #[arg(hide = true)]
    #[arg(global = true)]
    pub branch: Option<String>,

    /// Ask for password on terminal (TTY)
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(hide = true)]
    #[arg(global = true)]
    pub password: bool,

    /// Don't ask for password
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(hide = true)]
    #[arg(global = true)]
    pub no_password: bool,

    /// Read password from stdin rather than TTY (useful for scripts)
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(hide = true)]
    #[arg(global = true)]
    pub password_from_stdin: bool,

    /// Secret key to authenticate with
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(hide = true)]
    #[arg(global = true)]
    pub secret_key: Option<String>,

    /// Certificate to match server against
    ///
    /// Might either be a full self-signed server certificate or certificate
    /// authority (CA) certificate that the server certificate is signed with.
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(hide = true)]
    #[arg(global = true)]
    pub tls_ca_file: Option<PathBuf>,

    /// Verify server hostname using provided certificate.
    ///
    /// Useful when certificate authority (CA) is used for certificate
    /// handling and usually not used for self-signed certificates.
    ///
    /// Enabled by default when no specific certificate is present
    /// (via `--tls-ca-file` or in credentials JSON file)
    #[arg(long, hide = true)]
    #[arg(conflicts_with_all=&["no_tls_verify_hostname"])]
    #[arg(global = true)]
    pub tls_verify_hostname: bool, // deprecated for tls_security

    /// Do not verify server hostname
    ///
    /// This allows using any certificate for any hostname. However,
    /// a certificate must be present and matching certificate specified with
    /// `--tls-ca-file` or credentials file or signed by one of the root
    /// certificate authorities.
    #[arg(long, hide = true)]
    #[arg(conflicts_with_all=&["tls_verify_hostname"])]
    #[arg(global = true)]
    pub no_tls_verify_hostname: bool, // deprecated for tls_security

    /// Specifications for client-side TLS security mode:
    ///
    /// `insecure`:
    /// Do not verify server certificate at all, only use encryption.
    ///
    /// `no_host_verification`:
    /// This allows using any certificate for any hostname. However,
    /// a certificate must be present and matching certificate specified with
    /// `--tls-ca-file` or credentials file or signed by one of the root
    /// certificate authorities.
    ///
    /// `strict`:
    /// Verify server certificate and check hostname.
    /// Useful when certificate authority (CA) is used for certificate
    /// handling and usually not used for self-signed certificates.
    ///
    /// `default`:
    /// Defaults to "strict" when no specific certificate is present
    /// (via `--tls-ca-file` or in credentials JSON file); otherwise
    /// to "no_host_verification".
    #[arg(long, hide=true, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(value_name = "insecure | no_host_verification | strict | default")]
    #[arg(global = true)]
    pub tls_security: Option<String>,

    /// Override server name used for TLS connections and certificate
    /// verification.
    ///
    /// Useful when the server hostname cannot be used as it
    /// does not resolve, or resolves to a wrong IP address,
    /// and a different name or IP address is used in `--host`.
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP),)]
    #[arg(global = true)]
    pub tls_server_name: Option<String>,

    /// Retry up to WAIT_TIME (e.g. '30s') in case Gel connection
    /// cannot be established.
    #[arg(
        long,
        value_name="WAIT_TIME",
        help_heading=Some(CONN_OPTIONS_GROUP),
        value_parser=parse_duration,
    )]
    #[arg(hide = true)]
    #[arg(global = true)]
    pub wait_until_available: Option<Duration>,

    /// Connect to a passwordless Unix socket with superuser
    /// privileges by default.
    #[arg(long, hide=true, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(global = true)]
    pub admin: bool,

    /// Fail when no response from Gel for TIMEOUT (default '10s');
    /// alternatively will retry if `--wait-until-available` is also specified.
    #[arg(
        long,
        value_name="TIMEOUT",
        help_heading=Some(CONN_OPTIONS_GROUP),
        value_parser=parse_duration,
    )]
    #[arg(hide = true)]
    #[arg(global = true)]
    pub connect_timeout: Option<Duration>,
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct InstanceOptionsGlobal {
    /// Instance name (use [`BRANDING_CLI_CMD`] `instance list` to list local, remote and
    /// [`BRANDING_CLOUD`] instances available to you).
    #[arg(short='I', long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(value_hint=clap::ValueHint::Other)] // TODO complete instance name
    #[arg(global = true)]
    pub instance: Option<InstanceName>,

    /// Connect to a `docker` instance. If `docker-compose.yaml` is present,
    /// the instance will be automatically detected. Otherwise, `--container`
    /// must be specified.
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(conflicts_with_all=&["dsn", "credentials_file", "instance", "host", "unix_path"])]
    #[arg(global = true)]
    pub docker: bool,

    /// Connect to a specific `docker` container.
    #[arg(long, help_heading=Some(CONN_OPTIONS_GROUP))]
    #[arg(conflicts_with_all=&["dsn", "credentials_file", "instance", "host", "unix_path"])]
    #[arg(requires = "docker")]
    #[arg(global = true)]
    pub container: Option<String>,
}

impl InstanceOptionsGlobal {
    pub fn maybe_instance(&self) -> Option<InstanceName> {
        self.instance.clone()
    }
}

#[derive(clap::Args, Clone, Default, Debug, IntoArgs)]
pub struct InstanceOptions {
    #[arg(from_global)]
    pub instance: Option<InstanceName>,

    #[arg(from_global)]
    pub docker: bool,

    #[arg(from_global)]
    pub container: Option<String>,
}

#[derive(clap::Args, Clone, Default, Debug, IntoArgs)]
pub struct InstanceOptionsLegacy {
    /// Name of the instance
    #[arg(hide = true)]
    #[arg(value_hint=clap::ValueHint::Other)]
    pub name: Option<InstanceName>,

    #[command(flatten)]
    pub instance_opts: InstanceOptions,
}

impl From<InstanceName> for InstanceOptions {
    fn from(value: InstanceName) -> Self {
        InstanceOptions {
            instance: value.into(),
            ..Default::default()
        }
    }
}

impl From<InstanceName> for InstanceOptionsLegacy {
    fn from(value: InstanceName) -> Self {
        InstanceOptionsLegacy {
            instance_opts: value.into(),
            ..Default::default()
        }
    }
}

impl InstanceOptions {
    pub fn instance(&self) -> anyhow::Result<InstanceName> {
        if let Some(name) = &self.instance {
            return Ok(name.clone());
        }

        {
            // infer instance from current project
            let config = gel_tokio::Builder::new().build();
            if let Ok(config) = config {
                let instance = config.instance_name().cloned();

                if let Some(instance) = instance {
                    return Ok(instance);
                }
            }
        };

        // if we have docker, suggest linking it for this command
        if has_docker_blocking() {
            msg!(
                "{} Instance name argument is required, use '-I name'.",
                err_marker()
            );
            msg!(
                "{} `docker-compose.yaml` found. Use `{BRANDING_CLI_CMD} instance link --docker` to link it.",
                err_marker()
            );
            return Err(ExitCode::new(2).into());
        }

        msg!(
            "{} Instance name argument is required, use '-I name'",
            err_marker()
        );
        Err(ExitCode::new(2).into())
    }

    pub fn maybe_instance(&self) -> Option<InstanceName> {
        self.instance.clone()
    }
}

impl InstanceOptionsLegacy {
    pub fn instance(&self) -> anyhow::Result<InstanceName> {
        if let Some(name) = &self.name {
            print::error!(
                "Support for specifying an instance name as a positional argument has been removed. \
                Use `-I {name}` instead."
            );
            return Err(ExitCode::new(1).into());
        }

        self.instance_opts.instance()
    }

    pub fn instance_allow_legacy(&self) -> anyhow::Result<InstanceName> {
        if let Some(name) = &self.name {
            warn!("Instance name as a positional argument is deprecated, use '-I {name}' instead");
            return Ok(name.clone());
        }

        self.instance_opts.instance()
    }
}

impl ConnectionOptions {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.database.is_some() {
            print::warn!("database connection argument is deprecated in favor of 'branch'");
        }
        if let Some((d, b)) = self.database.as_ref().zip(self.branch.as_ref()) {
            anyhow::bail!("Arguments --database={d} and --branch={b} are mutually exclusive");
        }
        Ok(())
    }
}

#[derive(clap::Parser, Debug)]
#[command(disable_version_flag = true)]
pub struct HelpConnect {
    #[command(flatten)]
    pub conn: ConnectionOptions,
}

#[derive(clap::Args, IntoArgs, Clone, Debug)]
#[group(id = "cloudopts")]
pub struct CloudOptions {
    /// Specify the API endpoint. Defaults to the current logged-in
    /// server, or <https://api.g.aws.edgedb.cloud> if unauthorized
    #[arg(long, value_name="URL", help_heading=Some(CLOUD_OPTIONS_GROUP))]
    #[arg(global = true)]
    pub cloud_api_endpoint: Option<String>,

    /// Specify the API secret key to use instead of loading
    /// key from a remembered authentication.
    #[arg(long, value_name="SECRET_KEY", help_heading=Some(CLOUD_OPTIONS_GROUP))]
    #[arg(global = true)]
    pub cloud_secret_key: Option<String>,

    /// Specify the authenticated profile. Defaults to "default".
    #[arg(long, value_name="PROFILE", help_heading=Some(CLOUD_OPTIONS_GROUP))]
    #[arg(global = true)]
    pub cloud_profile: Option<String>,
}

/// Use the `gel` command-line tool to spin up local instances,
/// manage Gel projects, create and apply migrations, and more.
///
/// Running `gel` without a subcommand opens an interactive shell
/// for the instance in your directory. If you have no existing instance,
/// type `gel project init` to create one.
#[derive(clap::Parser, Debug)]
#[command(disable_version_flag = true)]
pub struct RawOptions {
    #[arg(long, hide = true)]
    pub debug_print_frames: bool,

    #[arg(long, hide = true)]
    pub debug_print_descriptors: bool,

    #[arg(long, hide = true)]
    pub debug_print_codecs: bool,

    #[arg(long, hide = true)]
    pub test_output_conn_params: bool,

    /// Print all available connection options
    /// for interactive shell along with subcommands
    #[arg(long)]
    pub help_connect: bool,

    /// Tab-separated output for queries
    #[arg(short = 't', long, overrides_with = "json", hide = true)]
    pub tab_separated: bool,
    /// JSON output for queries (single JSON list per query)
    #[arg(short = 'j', long, overrides_with = "tab_separated", hide = true)]
    pub json: bool,
    /// Execute a query instead of starting REPL
    #[arg(short = 'c', hide = true)]
    pub query: Option<String>,

    /// Show command-line tool version
    #[arg(short = 'V', long = "version")]
    pub print_version: bool,

    // Deprecated: use "no_cli_update_check" instead
    #[arg(long, hide = true)]
    pub no_version_check: bool,

    /// Disable check for new available CLI version
    #[arg(long)]
    pub no_cli_update_check: bool,

    /// Do not run any command hooks defined in [MANIFEST_FILE_DISPLAY_NAME]
    #[arg(long)]
    pub skip_hooks: bool,

    #[command(flatten)]
    pub conn: ConnectionOptions,

    #[command(flatten)]
    pub cloud: CloudOptions,
}

#[derive(clap::Args, Debug)]
pub struct SubcommandOption {
    #[command(subcommand)]
    pub subcommand: Option<Command>,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum Command {
    /// Initialize project (alias for [`BRANDING_CLI_CMD`] project init)
    Init(project::init::Command),
    /// Synchronize project with the current directory
    Sync(project::sync::Command),
    #[command(flatten)]
    Common(Common),
    /// Execute EdgeQL query in quotes (e.g. `"select 9;"`)
    Query(Query),
    /// Launch [`BRANDING`] instance in browser web UI
    UI(UI),
    /// Show paths for [`BRANDING`] installation
    Info(Info),
    /// Manage project installation
    Project(project::Command),
    /// Manage local [`BRANDING`] instances
    Instance(portable::instance::Command),
    /// Manage local [`BRANDING`] installations
    Server(portable::server::Command),
    /// Manage extensions of local instances
    Extension(portable::extension::Command),
    /// Generate shell completions
    #[command(name = "_gen_completions")]
    #[command(hide = true)]
    _GenCompletions(cli::gen_completions::Command),
    /// Self-installation commands
    #[command(name = "cli")]
    Cli(cli::Command),
    /// Install [`BRANDING`]
    #[command(name = "_self_install")]
    #[command(hide = true)]
    _SelfInstall(cli::install::Command),
    /// [`BRANDING_CLOUD`] authentication
    Cloud(CloudCommand),
    /// Start a long-running process that watches the project directory
    /// and runs scripts
    Watch(watch::Command),
    /// Generate a `SCRAM-SHA-256` hash for a password.
    HashPassword(HashPasswordCommand),
}

#[derive(clap::Args, Clone, Debug)]
pub struct Query {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    /// Output format: `json`, `json-pretty`, `json-lines`, `tab-separated`.
    /// Default is `json-pretty`.
    // todo: can't use `arg(default='json-pretty')` just yet, as we
    // need to see if the user did actually specify some output
    // format or not. We need that to support the now deprecated
    // --json and --tab-separated top-level options.
    #[arg(short = 'F', long)]
    pub output_format: Option<OutputFormat>,

    /// Input language: `edgeql`, `sql`.
    /// Default is `edgeql`.
    #[arg(short = 'L', long)]
    pub input_language: Option<InputLanguage>,

    /// Filename to execute queries from.
    /// Pass `--file -` to execute queries from stdin.
    #[arg(short = 'f', long)]
    pub file: Option<String>,

    pub queries: Option<Vec<String>>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct UI {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    /// Print URL in console instead of opening in the browser
    #[arg(long)]
    pub print_url: bool,

    /// Do not probe the UI endpoint of the server instance
    #[arg(long)]
    pub no_server_check: bool,
}

#[derive(clap::Args, Debug, Clone)]
pub struct Info {
    #[arg(long, value_parser=[
        "install-dir",
        "config-dir",
        "cache-dir",
        "data-dir",
        "service-dir",
    ])]
    /// Get specific value:
    ///
    /// * `install-dir` -- Directory where Gel CLI is installed
    /// * `config-dir` -- Base configuration directory
    /// * `cache-dir` -- Base cache directory
    /// * `data-dir` -- Base data directory (except on Windows)
    /// * `service-dir` -- Directory where supervisor/startup files are placed
    pub get: Option<String>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct HashPasswordCommand {
    pub password_to_hash: String,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub conn_options: ConnectionOptions,
    pub cloud_options: CloudOptions,
    pub subcommand: Option<Command>,
    pub interactive: bool,
    pub debug_print_frames: bool,
    pub debug_print_descriptors: bool,
    pub debug_print_codecs: bool,
    pub input_language: Option<InputLanguage>,
    pub output_format: Option<OutputFormat>,
    pub sql_output_format: Option<OutputFormat>,
    pub no_cli_update_check: bool,
    pub skip_hooks: bool,
    pub test_output_conn_params: bool,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("error: {}", msg)]
pub struct UsageError {
    kind: clap::error::ErrorKind,
    msg: String,
}

impl UsageError {
    pub fn new(kind: clap::error::ErrorKind, msg: impl std::fmt::Display) -> Self {
        UsageError {
            kind,
            msg: msg.to_string(),
        }
    }
    pub fn exit(&self) -> ! {
        clap::Error::raw(self.kind, &self.msg).exit()
    }
}

fn parse_duration(value: &str) -> anyhow::Result<Duration> {
    let value = value.parse::<model::Duration>()?;
    match value.is_negative() {
        false => Ok(value.abs_duration()),
        true => anyhow::bail!("negative durations are unsupported"),
    }
}

fn say_option_is_deprecated(option_name: &str, suggestion: &str) {
    let error = "warning:".to_string().emphasized().warning();
    let instead = suggestion.to_string().success();
    eprintln!(
        "\
        {error} The '{opt}' option is deprecated.\n\
        \n         \
            Use '{instead}' instead.\
        \n\
    ",
        error = error,
        opt = option_name.success(),
        instead = instead
    );
}

fn make_subcommand_help(parent: &clap::Command) -> String {
    use std::fmt::Write;

    let width = term_width();

    // When the terminal is wider than 82 characters clap aligns
    // the flags description text to the right of the flag name,
    // when it is narrower than 82, the description goes below
    // the option name.  We want to align the subcommand description
    // with the option description, hence there's some hand-tuning
    // of the padding here.
    let padding: usize = if width > 82 { 26 } else { 24 };

    let extra_padding: usize = 4 + 1;
    let details_width: usize = width - padding - extra_padding;

    let wrap = |text: &str| {
        if text.len() <= details_width {
            return text.to_string();
        }

        let text = textwrap::fill(text, details_width);
        let mut lines = text.lines();
        let mut new_lines = vec![lines.next().unwrap().to_string()];
        for line in lines {
            new_lines.push(format!("  {:padding$} {}", " ", line, padding = padding));
        }

        new_lines.join("\n")
    };

    let mut buf = String::with_capacity(4096);

    write!(
        &mut buf,
        color_print::cstr!("<bold><underline>Commands</underline></bold>:\n"),
    )
    .unwrap();
    let mut empty_line = true;

    for cmd in parent.get_subcommands() {
        if cmd.is_hide_set() {
            continue;
        }
        if cmd.get_version() == Some("help_expand") {
            if !empty_line {
                buf.push('\n');
            }
            for subcmd in cmd.get_subcommands() {
                if subcmd.is_hide_set() {
                    continue;
                }
                writeln!(
                    &mut buf,
                    "  {} {}",
                    color_print::cformat!(
                        "<bold>{:padding$}</bold>",
                        format!("{} {}", cmd.get_name(), subcmd.get_name()),
                        padding = padding,
                    ),
                    wrap(
                        &subcmd
                            .get_about()
                            .or_else(|| subcmd.get_long_about())
                            .unwrap_or_default()
                            .ansi()
                            .to_string()
                    ),
                )
                .unwrap();
            }
            buf.push('\n');
            empty_line = true;
        } else {
            let name = if cmd.has_subcommands() {
                format!("{} ...", cmd.get_name())
            } else {
                cmd.get_name().to_string()
            };
            writeln!(
                &mut buf,
                "  {} {}",
                color_print::cformat!("<bold>{:padding$}</bold>", name, padding = padding,),
                wrap(
                    &cmd.get_about()
                        .or_else(|| cmd.get_long_about())
                        .unwrap_or_default()
                        .ansi()
                        .to_string()
                ),
            )
            .unwrap();
            empty_line = false;
        }
    }
    buf.truncate(buf.trim_end().len());

    buf
}

/// Swap the standard subcommand help with expanded subcommand help.
fn update_main_help(mut app: clap::Command) -> clap::Command {
    if !print::use_color() {
        app = app.color(clap::ColorChoice::Never);
    }
    let sub_cmd = make_subcommand_help(&app);

    let help = format!("{}", app.render_help().ansi()).to_string();
    let subcmd_index = help.find("Commands:").unwrap();
    let opt_index = help.find("Options:").unwrap();

    let help = [
        &help[..subcmd_index],
        &sub_cmd,
        &color_print::cformat!("\n\n<bold><underline>Options:</underline></bold>"),
        &help[(opt_index + 8)..],
    ]
    .join("");

    let help = std::str::from_utf8(Vec::leak(help.into())).unwrap();
    app.override_help(help)
}

fn update_help_branding(help: &str) -> String {
    let mut help = help.to_string();

    for (placeholder, value) in [
        ("BRANDING", BRANDING),
        ("BRANDING_CLI_CMD", BRANDING_CLI_CMD),
        ("BRANDING_CLOUD", BRANDING_CLOUD),
    ] {
        let value = cformat!("<bold>{}</bold>", value);
        let pattern1 = format!("[{placeholder}]");
        help = help.replace(&pattern1, &value);
        let pattern2 = format!("[`{placeholder}`]");
        help = help.replace(&pattern2, &value);
    }

    markdown::format_title(&help)
}

fn update_cmd_about(cmd: &mut clap::Command) {
    let mut new_cmd = cmd.clone();
    if let Some(about) = new_cmd.get_long_about() {
        let about = update_help_branding(&about.ansi().to_string());
        new_cmd = new_cmd.long_about(about);
    }
    if let Some(about) = new_cmd.get_about() {
        let about = update_help_branding(&about.ansi().to_string());
        new_cmd = new_cmd.about(about);
    }

    new_cmd = new_cmd.mut_args(|arg| {
        let mut arg = arg;
        if let Some(about) = arg.get_help() {
            let about = update_help_branding(&about.ansi().to_string());
            arg = arg.help(about);
        }
        arg
    });

    *cmd = new_cmd;

    for subcmd in cmd.get_subcommands_mut() {
        update_cmd_about(subcmd);
    }
}

fn print_full_connection_options() {
    let mut app = <HelpConnect as clap::CommandFactory>::command();
    update_cmd_about(&mut app);

    let mut new_app = clap::Command::new("edgedb-connect").term_width(term_width());
    if !print::use_color() {
        new_app = new_app.color(clap::ColorChoice::Never);
    }

    for arg in app.get_arguments() {
        let arg_name = arg.get_id();
        if arg_name == "help" || arg_name == "version" || arg_name == "admin" {
            continue;
        }
        let new_arg = arg.clone().hide(false);
        new_app = new_app.arg(new_arg);
    }

    // "Long help" has more whitespace and is much more readable
    // for the many options we have in the connection group.
    let help = format!("{}", new_app.render_long_help().ansi());
    let subcmd_index = help.find(CONN_OPTIONS_GROUP).unwrap();
    let slice_from = subcmd_index + CONN_OPTIONS_GROUP.len() + 1;
    let help = &help[slice_from..];

    color_print::cprintln!("<bold><underline>Connection Options (full list):</underline></bold>");
    println!("{help}");
}

fn term_width() -> usize {
    // clap::Command::max_term_width() works poorly in conjunction
    // with  clap::Command::term_width(); it appears that one call
    // disables the effect of the other. Therefore we want to
    // calculate the acceptable term width ourselves and use
    // that to configure clap and to render subcommands help.

    let width = terminal_size::terminal_size().map_or(80, |(terminal_size::Width(w), _)| w.into());

    width.clamp(MIN_TERM_WIDTH, MAX_TERM_WIDTH)
}

impl Options {
    pub fn error(&self, kind: clap::error::ErrorKind, msg: impl std::fmt::Display) -> UsageError {
        UsageError::new(kind, msg)
    }

    pub fn command() -> clap::Command {
        // Connection/Cloud options apply *both* to the
        // root command when ran without arguments (i.e. REPL mode)
        // and to many, but, crucially, not ALL subcommands, so
        // we cannot simply make ConnectionOptions and CloudOptions
        // global at the top level.  Instead we create a copy of those
        // groups here and deglobalize the arguments before adding
        // subcommands.  Various subcommand trees should then add
        //
        //    #[command(flatten)]
        //    pub conn: ConnectionOptions,
        //
        // to enable connection and/or cloud options for themselves
        // and their subcommands.
        let tmp = clap::Command::new(BRANDING_CLI_CMD);
        let tmp = <RawOptions as clap::Args>::augment_args(tmp);
        let mut global_args: Vec<_> = tmp
            .get_groups()
            .filter(|g| g.get_id() == "connopts" || g.get_id() == "cloudopts")
            .flat_map(|g| g.get_args())
            .collect();
        global_args.sort_unstable();

        let deglobalized = tmp.get_arguments().map(|arg| {
            if global_args.binary_search(&arg.get_id()).is_ok() {
                arg.clone().global(false)
            } else {
                arg.clone()
            }
        });

        let app = clap::Command::new(BRANDING_CLI_CMD)
            .term_width(term_width())
            .args(deglobalized);

        let mut app = <SubcommandOption as clap::Args>::augment_args(app);
        update_cmd_about(&mut app);
        update_main_help(app)
    }

    pub fn from_args_and_env() -> anyhow::Result<Options> {
        let app = Options::command();
        let matches = app.clone().get_matches();
        let args = <RawOptions as clap::FromArgMatches>::from_arg_matches(&matches)?;
        let cmd = <SubcommandOption as clap::FromArgMatches>::from_arg_matches(&matches)?;

        let subcommand = cmd.subcommand;

        if args.help_connect {
            print_full_connection_options();
            return Err(ExitCode::new(0).into());
        }

        if args.print_version {
            println!("{BRANDING} CLI {}", clap::crate_version!());
            return Err(ExitCode::new(0).into());
        }

        if subcommand.is_some() && args.query.is_some() {
            anyhow::bail!("Option `-c` conflicts with specifying a subcommand");
        }

        let is_ci = *cli::env::Env::in_ci()?.unwrap_or_default();
        if is_ci {
            let msg = "Running in CI mode (CI=true), assuming \
                --no-cli-update-check, GEL_AUTO_BACKUP_MODE=disabled"
                .muted();
            eprintln!("{msg}");
        }

        // TODO(pc) add option to force interactive mode not on a tty (tests)
        let interactive =
            args.query.is_none() && subcommand.is_none() && !is_ci && stdin().is_terminal();

        if args.json {
            say_option_is_deprecated(
                "--json",
                concatcp!(BRANDING_CLI_CMD, " query --output-format=json"),
            );
        }
        if args.tab_separated {
            say_option_is_deprecated(
                "--tab-separated",
                "edgedb query --output-format=tab-separated",
            );
        }
        let subcommand = if let Some(query) = args.query {
            say_option_is_deprecated("-c", concatcp!(BRANDING_CLI_CMD, " query"));
            let output_format = if args.json {
                Some(OutputFormat::Json)
            } else if args.tab_separated {
                Some(OutputFormat::TabSeparated)
            } else {
                Some(OutputFormat::JsonPretty)
            };
            Some(Command::Query(Query {
                queries: Some(vec![query]),
                output_format,
                input_language: Some(InputLanguage::EdgeQl),
                file: None,
                conn: args.conn.clone(),
            }))
        } else {
            subcommand
        };

        let mut no_cli_update_check = args.no_cli_update_check || is_ci;
        if args.no_version_check {
            no_cli_update_check = true;
            let warning = "warning:".to_string().emphasized().warning();
            eprintln!(
                "\
                {warning} The '--no-version-check' option was renamed.\n\
                \n         \
                    Use '--no-cli-update-check' instead.\
                \n\
            "
            );
        }

        let mut skip_hooks = args.skip_hooks;
        if !skip_hooks && crate::cli::env::Env::skip_hooks()?.is_some_and(|x| x.0) {
            skip_hooks = true;
        }

        Ok(Options {
            conn_options: args.conn,
            cloud_options: args.cloud,
            interactive,
            subcommand,
            debug_print_frames: args.debug_print_frames,
            debug_print_descriptors: args.debug_print_descriptors,
            debug_print_codecs: args.debug_print_codecs,
            input_language: Some(InputLanguage::EdgeQl),
            output_format: if args.tab_separated {
                Some(OutputFormat::TabSeparated)
            } else if args.json {
                Some(OutputFormat::Json)
            } else {
                None
            },
            sql_output_format: None,
            no_cli_update_check,
            skip_hooks,
            test_output_conn_params: args.test_output_conn_params,
        })
    }

    pub async fn create_connector(&self) -> anyhow::Result<Connector> {
        let mut builder = prepare_conn_params(self).await?;
        if self.conn_options.password_from_stdin || self.conn_options.password {
            // Temporary set an empty password. It will be overriden by
            // `config.with_password()` but we need it here so that
            // `gel://?password_env=NON_EXISTING` does not read the
            // environemnt variable
            builder = builder.password("");
        }
        match builder.clone().build() {
            Ok(mut config) => {
                if let Some(password) = with_password(&self.conn_options, config.user()).await? {
                    config = config.with_password(&password);
                }
                Ok(Connector::new(Ok(config)))
            }
            Err(e) => {
                if e.is::<NoCloudConfigFound>() {
                    return Err(anyhow::anyhow!(
                        "No {BRANDING_CLOUD} configuration found, but a {BRANDING_CLOUD} instance was specified."
                    ))
                    .with_hint(|| {
                        format!(
                            "Please run `{BRANDING_CLI_CMD} cloud login` to use {BRANDING_CLOUD} instances."
                        )
                    })
                    .map_err(Into::into);
                }

                let (cfg, errors) = builder.clone().compute()?;

                // ask password anyways, so input that fed as a password
                // never goes to anywhere else
                with_password(
                    &self.conn_options,
                    &cfg.user.unwrap_or("edgedb".to_string()),
                )
                .await?;

                if e.is::<ClientNoCredentialsError>() {
                    if let Ok(builder) = try_docker_fallback(builder).await {
                        if let Ok(config) = builder.build() {
                            return Ok(Connector::new(Ok(config)));
                        }
                    }

                    let project = project::find_project_async(None).await?;
                    let message = if let Some(project) = project {
                        format!(
                            "found project at {}, but it is not initialized and no connection options \
                            are specified: {errors:?}",
                            project.root.as_relative().display()
                        )
                    } else {
                        format!(
                            "no {MANIFEST_FILE_DISPLAY_NAME} found and no connection options \
                            are specified"
                        )
                    };
                    Ok(Connector::new(
                        Err(anyhow::anyhow!(message))
                            .hint(CONNECTION_ARG_HINT)
                            .map_err(Into::into),
                    ))
                } else {
                    Ok(Connector::new(Err(e.into())))
                }
            }
        }
    }

    #[tokio::main(flavor = "current_thread")]
    pub async fn block_on_create_connector(&self) -> anyhow::Result<Connector> {
        self.create_connector().await
    }
}

async fn with_password(options: &ConnectionOptions, user: &str) -> anyhow::Result<Option<String>> {
    if options.password_from_stdin {
        let password = unblock(tty_password::read_stdin).await??;
        Ok(Some(password))
    } else if options.no_password {
        Ok(None)
    } else if options.password {
        let user = user.to_string();
        let password = unblock(move || {
            let user = user.escape_default();
            tty_password::read(format!("Password for '{user}': "))
        })
        .await??;
        Ok(Some(password))
    } else {
        Ok(None)
    }
}

pub async fn prepare_conn_params(opts: &Options) -> anyhow::Result<Builder> {
    let tmp = &opts.conn_options;
    let mut bld = Builder::new();
    let instance = tmp.instance_opts.maybe_instance();

    if opts.conn_options.instance_opts.docker {
        if let Some(container) = &opts.conn_options.instance_opts.container {
            bld = try_docker(bld, DockerMode::ExplicitInstance(container.clone())).await?;
        } else {
            bld = try_docker(bld, DockerMode::ExplicitAuto).await?;
        }
    }

    if tmp.admin {
        if tmp.unix_path.is_none() && instance.is_none() {
            anyhow::bail!("`--admin` requires `--unix-path` or `--instance`");
        }
        if let Some(unix_path) = &tmp.unix_path {
            bld = bld.unix_path(UnixPath::with_port_suffix(
                unix_path.join(".s.EDGEDB.admin."),
            ));
        }
        if let Some(instance) = &instance {
            let InstanceName::Local(name) = &instance else {
                anyhow::bail!("`--instance` must be a local instance name when using `--admin`");
            };
            let sock = runstate_dir(name)?.join(".s.EDGEDB.admin.");
            bld = bld.unix_path(UnixPath::with_port_suffix(sock));
        }
    }
    if let Some(host) = &tmp.host {
        if host.contains('/') {
            anyhow::bail!(
                "`--host` must be a hostname or IP address, not a path to a unix socket.
                Use `--admin` and `--unix-path` or `--instance` to administer a local instance."
            );
        } else {
            bld = bld.host_string(host);
        }
    }
    if let Some(port) = tmp.port {
        bld = bld.port(port);
    }
    if let Some(dsn) = &tmp.dsn {
        bld = bld.dsn(dsn);
    }
    if let Some(instance) = &instance {
        bld = bld.instance(instance.clone());
    }
    if let Some(secret_key) = &tmp.secret_key {
        bld = bld.secret_key(secret_key);
    }
    if let Some(file_path) = &tmp.credentials_file {
        bld = bld.credentials_file(file_path);
    }
    if let Some(user) = &tmp.user {
        bld = bld.user(user);
    }
    if let Some(val) = tmp.wait_until_available {
        bld = bld.wait_until_available(val);
    }
    if let Some(val) = tmp.connect_timeout {
        bld = bld.connect_timeout(val);
    }
    if let Some(val) = &tmp.secret_key {
        bld = bld.secret_key(val);
    }
    if let Some(database) = &tmp.database {
        bld = bld.database(database);
        bld = bld.branch(database);
    } else if let Some(branch) = &tmp.branch {
        bld = bld.branch(branch);
        bld = bld.database(branch);
    }

    bld = load_tls_options(tmp, bld)?;
    Ok(bld)
}

pub fn load_tls_options(
    options: &ConnectionOptions,
    mut builder: Builder,
) -> anyhow::Result<Builder> {
    if let Some(cert_file) = &options.tls_ca_file {
        builder = builder.tls_ca_file(cert_file);
    }
    let mut security = match options.tls_security.as_deref() {
        None => None,
        Some("insecure") => Some(TlsSecurity::Insecure),
        Some("no_host_verification") => Some(TlsSecurity::NoHostVerification),
        Some("strict") => Some(TlsSecurity::Strict),
        Some("default") => Some(TlsSecurity::Default),
        Some(_) => anyhow::bail!(
            "Unsupported TLS security, options: \
             `default`, `strict`, `no_host_verification`, `insecure`"
        ),
    };
    if options.no_tls_verify_hostname {
        if let Some(s) = security {
            if s != TlsSecurity::NoHostVerification {
                anyhow::bail!(
                    "Cannot set --no-tls-verify-hostname while \
                     --tls-security is also set"
                );
            }
        } else {
            security = Some(TlsSecurity::NoHostVerification);
        }
    }
    if options.tls_verify_hostname {
        if let Some(s) = security {
            if s != TlsSecurity::Strict {
                anyhow::bail!(
                    "Cannot set --tls-verify-hostname while \
                     --tls-security is also set"
                );
            }
        } else {
            security = Some(TlsSecurity::Strict);
        }
    }
    if let Some(s) = security {
        builder = builder.tls_security(s);
    }
    if let Some(tls_server_name) = &options.tls_server_name {
        builder = builder.tls_server_name(tls_server_name);
    }
    Ok(builder)
}
