use std::fs;
use std::str::FromStr;

use anyhow::Context;
use const_format::concatcp;
use edgedb_cli_derive::IntoArgs;
use fn_error_context::context;

use color_print::cformat;
use serde::{Deserialize, Serialize};

use crate::branding::{
    BRANDING, BRANDING_CLI_CMD, BRANDING_CLOUD, BRANDING_DEFAULT_USERNAME,
    BRANDING_DEFAULT_USERNAME_LEGACY,
};
use crate::cloud;
use crate::commands::ExitCode;
use crate::credentials;
use crate::hint::HintExt;
use crate::options::CloudOptions;
use crate::platform;
use crate::portable::instance::control::Start;
use crate::portable::instance::control::{self, ensure_runstate_dir, self_signed_arg};
use crate::portable::instance::reset_password::{generate_password, password_hash};
use crate::portable::local::{InstanceInfo, Paths};
use crate::portable::local::{allocate_port, write_json};
use crate::portable::options::{CloudInstanceParams, InstanceName};
use crate::portable::platform::optional_docker_check;
use crate::portable::repository::{Channel, Query, QueryOptions};
use crate::portable::server::install;
use crate::portable::ver::Specific;
use crate::portable::{exit_codes, ver};
use crate::portable::{linux, macos, windows};
use crate::print::{self, Highlight, err_marker, msg};
use crate::process::{self, IntoArg};
use crate::question;

use gel_tokio::credentials::Credentials;

pub fn run(cmd: &Command, opts: &crate::options::Options) -> anyhow::Result<()> {
    if optional_docker_check()? {
        print::error!(
            "`{BRANDING_CLI_CMD} instance create` is not supported in Docker containers."
        );
        Err(ExitCode::new(exit_codes::DOCKER_CONTAINER))?;
    }
    if cmd.start_conf.is_some() {
        print::warn!(
            "The option `--start-conf` is deprecated. \
                     Use `{BRANDING_CLI_CMD} instance start/stop` to control \
                     the instance."
        );
    }

    let mut client = cloud::client::CloudClient::new(&opts.cloud_options)?;
    let inst_name = if let Some(name) = cmd.name.as_ref().or(cmd.instance.as_ref()) {
        name.to_owned()
    } else if cmd.non_interactive {
        msg!(
            "{} Instance name is required \
                             in non-interactive mode",
            err_marker()
        );
        return Err(ExitCode::new(2).into());
    } else {
        ask_name(&mut client)?
    };

    let name = match inst_name.clone() {
        InstanceName::Local(name) => name,
        InstanceName::Cloud { org_slug, name } => {
            create_cloud(cmd, opts, &org_slug, &name, &client)?;
            return Ok(());
        }
    };

    let cp = &cmd.cloud_params;

    if cp.region.is_some() {
        Err(opts.error(
            clap::error::ErrorKind::ArgumentConflict,
            cformat!(
                "The <bold>--region</bold> option is only applicable to {BRANDING_CLOUD} instances."
            ),
        ))?;
    }

    if cp.billables.compute_size.is_some() {
        Err(opts.error(
            clap::error::ErrorKind::ArgumentConflict,
            cformat!(
                "The <bold>--compute-size</bold> option is only applicable to {BRANDING_CLOUD} instances."
            ),
        ))?;
    }

    if cp.billables.storage_size.is_some() {
        Err(opts.error(
            clap::error::ErrorKind::ArgumentConflict,
            cformat!(
                "The <bold>--storage-size</bold> option is only applicable to {BRANDING_CLOUD} instances."
            ),
        ))?;
    }

    let paths = Paths::get(&name)?;
    paths
        .check_exists()
        .with_context(|| format!("instance {name:?} detected"))
        .with_hint(|| {
            format!(
                "Use `{BRANDING_CLI_CMD} instance destroy -I {name}` \
                              to remove rest of unused instance"
            )
        })?;

    let port = cmd.port.map(Ok).unwrap_or_else(|| allocate_port(&name))?;

    let info = if cfg!(windows) {
        windows::create_instance(cmd, &name, port, &paths)?;
        InstanceInfo {
            name: name.clone(),
            instance_name: inst_name,
            installation: None,
            port,
        }
    } else {
        let (query, _) = Query::from_options(
            QueryOptions {
                nightly: cmd.nightly,
                testing: false,
                channel: cmd.channel,
                version: cmd.version.as_ref(),
                stable: false,
            },
            || anyhow::Ok(Query::stable()),
        )?;
        let inst = install::version(&query).context(concatcp!("error installing ", BRANDING))?;
        let specific_version = &inst.version.specific();
        let info = InstanceInfo {
            name: name.clone(),
            instance_name: inst_name,
            installation: Some(inst),
            port,
        };
        bootstrap(
            &paths,
            &info,
            cmd.default_user
                .as_deref()
                .unwrap_or_else(|| get_default_user_name(specific_version)),
            &cmd.default_branch
                .clone()
                .unwrap_or_else(|| get_default_branch_name(specific_version)),
        )?;
        info
    };

    if windows::is_wrapped() {
        // no service and no messages
        return Ok(());
    }

    match create_service(&info) {
        Ok(()) => {}
        Err(e) => {
            log::warn!("Error running {BRANDING} as a service: {e:#}");
            print::warn!(
                "{BRANDING} will not start on next login. \
                         Trying to start database in the background..."
            );
            control::start(&Start {
                instance_opts: info.instance_name.into(),
                foreground: false,
                auto_restart: false,
                managed_by: None,
            })?;
        }
    }

    msg!("Instance {} is up and running.", name.clone().emphasized());
    msg!("To connect to the instance run:");
    msg!("  {BRANDING_CLI_CMD} -I {name}");
    Ok(())
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Name of instance to create. Asked interactively if not specified.
    #[arg(value_hint=clap::ValueHint::Other)]
    pub name: Option<InstanceName>,

    #[arg(from_global)]
    pub instance: Option<InstanceName>,

    /// Create instance using the latest nightly version.
    #[arg(long, conflicts_with_all=&["channel", "version"])]
    pub nightly: bool,
    /// Create instance with specified version.
    #[arg(long, conflicts_with_all=&["nightly", "channel"])]
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
    #[arg(long, hide = true)]
    pub start_conf: Option<StartConf>,

    /// Default user name (created during initialization and saved in
    /// credentials file). This defaults to 'admin' on EdgeDB >=6.x; otherwise
    /// 'edgedb' is used.
    #[arg(long)]
    pub default_user: Option<String>,

    /// The default branch name. This defaults to 'main' on EdgeDB >=5.x; otherwise
    /// 'edgedb' is used.
    pub default_branch: Option<String>,

    /// Do not ask questions. Assume user wants to upgrade instance.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum StartConf {
    Auto,
    Manual,
}

impl FromStr for StartConf {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<StartConf> {
        match s {
            "auto" => Ok(StartConf::Auto),
            "manual" => Ok(StartConf::Manual),
            _ => anyhow::bail!(
                "Unsupported start configuration, \
                options: `auto`, `manual`"
            ),
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

impl std::fmt::Display for StartConf {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.as_str().fmt(f)
    }
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

fn ask_name(cloud_client: &mut cloud::client::CloudClient) -> anyhow::Result<InstanceName> {
    let instances = credentials::all_instance_names()?;
    loop {
        let name = question::String::new("Specify a name for the new instance").ask()?;
        let inst_name = match InstanceName::from_str(&name) {
            Ok(name) => name,
            Err(e) => {
                print::error!("{e}");
                continue;
            }
        };
        let exists = match &inst_name {
            InstanceName::Local(name) => instances.contains(name),
            InstanceName::Cloud { org_slug, name } => {
                if !cloud_client.is_logged_in {
                    if let Err(e) = cloud::ops::prompt_cloud_login(cloud_client) {
                        print::error!("{e}");
                        continue;
                    }
                }
                cloud::ops::find_cloud_instance_by_name(name, org_slug, cloud_client)?.is_some()
            }
        };
        if exists {
            msg!(
                "{} Instance {} already exists.",
                err_marker(),
                name.emphasized()
            );
        } else {
            return Ok(inst_name);
        }
    }
}

fn create_cloud(
    cmd: &Command,
    opts: &crate::options::Options,
    org_slug: &str,
    name: &str,
    client: &cloud::client::CloudClient,
) -> anyhow::Result<()> {
    let inst_name = InstanceName::Cloud {
        org_slug: org_slug.to_string(),
        name: name.to_string(),
    };

    client.ensure_authenticated()?;

    let cp = &cmd.cloud_params;

    let region = match &cp.region {
        None => cloud::ops::get_current_region(client)?.name,
        Some(region) => region.to_string(),
    };

    let org = cloud::ops::get_org(org_slug, client)?;

    let (query, _) = Query::from_options(
        QueryOptions {
            nightly: cmd.nightly,
            testing: false,
            channel: cmd.channel,
            version: cmd.version.as_ref(),
            stable: false,
        },
        || anyhow::Ok(Query::stable()),
    )?;

    let server_ver = cloud::versions::get_version(&query, client)?;

    let compute_size = &cp.billables.compute_size;
    let storage_size = &cp.billables.storage_size;

    let tier = if let Some(tier) = cp.billables.tier {
        tier
    } else if compute_size.is_some()
        || storage_size.is_some()
        || org.preferred_payment_method.is_some()
    {
        cloud::ops::CloudTier::Pro
    } else {
        cloud::ops::CloudTier::Free
    };

    if tier == cloud::ops::CloudTier::Free {
        if compute_size.is_some() {
            Err(opts.error(
                clap::error::ErrorKind::ArgumentConflict,
                cformat!(
                    "The <bold>--compute-size</bold> option can \
                only be specified for Pro instances."
                ),
            ))?;
        }
        if storage_size.is_some() {
            Err(opts.error(
                clap::error::ErrorKind::ArgumentConflict,
                cformat!(
                    "The <bold>--storage-size</bold> option can \
                only be specified for Pro instances."
                ),
            ))?;
        }
    }

    let prices = cloud::ops::get_prices(client)?;
    let tier_prices = prices.get(&tier).context(format!(
        "could not download pricing information for the {tier} tier"
    ))?;
    let region_prices = tier_prices.get(&region).context(format!(
        "could not download pricing information for the {region} region"
    ))?;
    let default_compute = region_prices
        .iter()
        .find(|&price| price.billable == "compute")
        .context("could not download pricing information for compute")?
        .units_default
        .clone()
        .context("could not find default value for compute")?;

    let default_storage = region_prices
        .iter()
        .find(|&price| price.billable == "storage")
        .context("could not download pricing information for storage")?
        .units_default
        .clone()
        .context("could not find default value for storage")?;

    let mut req_resources: Vec<cloud::ops::CloudInstanceResourceRequest> = vec![];

    let compute_size_v = match compute_size {
        None => default_compute,
        Some(v) => v.clone(),
    };

    let storage_size_v = match storage_size {
        None => default_storage,
        Some(v) => v.clone(),
    };

    if compute_size.is_some() {
        req_resources.push(cloud::ops::CloudInstanceResourceRequest {
            name: "compute".to_string(),
            value: compute_size_v.clone(),
        });
    }

    if storage_size.is_some() {
        req_resources.push(cloud::ops::CloudInstanceResourceRequest {
            name: "storage".to_string(),
            value: storage_size_v.clone(),
        });
    }

    let resources_display = format!(
        "\nCompute Size: {} compute unit{}\
        \nStorage Size: {} gigabyte{}",
        compute_size_v,
        if compute_size_v == "1" { "" } else { "s" },
        storage_size_v,
        if storage_size_v == "1" { "" } else { "s" },
    );

    if !cmd.non_interactive
        && !question::Confirm::new(format!(
            "This will create a new {BRANDING_CLOUD} instance with the following parameters:\
        \n\
        \nTier: {tier:?}\
        \nRegion: {region}\
        \nServer Version: {server_ver}\
        {resources_display}\
        \n\nIs this acceptable?",
        ))
        .ask()?
    {
        return Ok(());
    }

    let source_instance_id = match &cmd.cloud_backup_source.from_instance {
        Some(InstanceName::Cloud {
            org_slug: org,
            name,
        }) => match cloud::ops::find_cloud_instance_by_name(name, org, client) {
            Ok(Some(instance)) => Ok(Some(instance.id)),
            Ok(None) => Err(opts.error(
                clap::error::ErrorKind::InvalidValue,
                cformat!(
                    "The instance specified by <bold>--from-instance</bold> does \
                        not exist or is inaccessible."
                ),
            ))?,
            Err(e) => Err(e),
        },
        Some(InstanceName::Local(_)) => Err(opts.error(
            clap::error::ErrorKind::InvalidValue,
            cformat!(
                "The instance specified by <bold>--from-instance</bold> does \
                not specify a valid {BRANDING_CLOUD} instance, a name in the 'org/instance' format is expected."
            ),
        ))?,
        None => Ok(None),
    }?;

    let request = cloud::ops::CloudInstanceCreate {
        name: name.to_string(),
        org: org_slug.to_string(),
        version: server_ver.to_string(),
        region: Some(region),
        requested_resources: Some(req_resources),
        tier: Some(tier),
        source_instance_id,
        source_backup_id: cmd.cloud_backup_source.from_backup_id.clone(),
    };
    cloud::ops::create_cloud_instance(client, &request)?;
    msg!("{BRANDING_CLOUD} instance {inst_name} is up and running.");
    msg!("To connect to the instance run:");
    msg!("  {BRANDING_CLI_CMD} -I {inst_name}");
    Ok(())
}

fn bootstrap_script(user: &str, password: &str, default_user: &str) -> String {
    use edgeql_parser::helpers::{quote_name, quote_string};
    use std::fmt::Write;

    let mut output = String::with_capacity(1024);
    if user == default_user {
        write!(
            &mut output,
            r###"
            ALTER ROLE {name} {{
                SET password_hash := {password_hash};
            }};
            "###,
            name = quote_name(user),
            password_hash = quote_string(&password_hash(password)),
        )
        .unwrap();
    } else {
        write!(
            &mut output,
            r###"
            CREATE SUPERUSER ROLE {name} {{
                SET password_hash := {password_hash};
            }}"###,
            name = quote_name(user),
            password_hash = quote_string(&password_hash(password)),
        )
        .unwrap();
    }
    output
}

#[context("cannot bootstrap {BRANDING} instance")]
pub fn bootstrap(
    paths: &Paths,
    info: &InstanceInfo,
    user: &str,
    branch: &str,
) -> anyhow::Result<()> {
    let server_path = info.server_path()?;

    let tmp_data = platform::tmp_file_path(&paths.data_dir);
    if tmp_data.exists() {
        fs::remove_dir_all(&tmp_data).with_context(|| format!("removing {:?}", &tmp_data))?;
    }
    fs::create_dir_all(&tmp_data).with_context(|| format!("creating {:?}", &tmp_data))?;

    let password = generate_password();
    let script = bootstrap_script(
        user,
        &password,
        // This is the user included in the server. It changed since 6.0-alpha.2.
        if info.get_version()?.specific() >= Specific::from_str("6.0-alpha.2").unwrap() {
            BRANDING_DEFAULT_USERNAME
        } else {
            BRANDING_DEFAULT_USERNAME_LEGACY
        },
    );

    msg!("Initializing {BRANDING} instance...");
    let mut cmd = process::Native::new("bootstrap", "edgedb", server_path);
    cmd.arg("--bootstrap-only");
    cmd.env_default("EDGEDB_SERVER_LOG_LEVEL", "warn");
    cmd.arg("--data-dir").arg(&tmp_data);
    cmd.arg("--runstate-dir")
        .arg(&ensure_runstate_dir(&info.name)?);
    self_signed_arg(&mut cmd, info.get_version()?);
    cmd.arg("--bootstrap-command").arg(script);
    if info.get_version()?.specific().major >= 5 {
        cmd.arg("--default-branch").arg(branch);
    } else {
        cmd.arg("--default-database").arg(branch);
    }
    cmd.run()?;

    let cert_path = tmp_data.join("edbtlscert.pem");
    let cert = fs::read_to_string(&cert_path)
        .with_context(|| format!("cannot read certificate: {cert_path:?}"))?;

    write_json(&tmp_data.join("instance_info.json"), "metadata", &info)?;
    fs::rename(&tmp_data, &paths.data_dir)
        .with_context(|| format!("renaming {:?} -> {:?}", tmp_data, paths.data_dir))?;

    let credentials = Credentials {
        user: user.into(),
        host: Some("localhost".to_string()),
        port: Some(info.port),
        database: Some(branch.to_string()),
        branch: Some(branch.to_string()),
        password: Some(password),
        tls_ca: Some(cert),
        tls_security: gel_dsn::gel::TlsSecurity::NoHostVerification,
        ..Default::default()
    };
    credentials::write(&paths.credentials, &credentials)?;
    Ok(())
}

pub fn create_service(meta: &InstanceInfo) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        macos::create_service(meta)
    } else if cfg!(target_os = "linux") {
        if windows::is_wrapped() {
            // No service. Managed by windows.
            // Note: in `create` we avoid even calling this function because
            // we need to print message from windows. But on upgrade, revert,
            // etc. completion message is printed from the linux, so this
            // function is called.
            Ok(())
        } else {
            linux::create_service(meta)
        }
    } else if cfg!(windows) {
        windows::create_service(meta)
    } else {
        anyhow::bail!("creating a service is not supported on the platform");
    }
}

pub fn get_default_branch_name(version: &Specific) -> String {
    if version.major >= 5 {
        return String::from("main");
    }

    String::from("edgedb")
}

pub fn get_default_user_name(version: &Specific) -> &'static str {
    if version.major >= 6 {
        BRANDING_DEFAULT_USERNAME
    } else {
        BRANDING_DEFAULT_USERNAME_LEGACY
    }
}
