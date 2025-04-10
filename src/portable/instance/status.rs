use std::collections::BTreeSet;
use std::fmt::Display;
use std::fs;
use std::future::{Future, pending};
use std::io;
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use fn_error_context::context;
use gel_cli_derive::IntoArgs;
use gel_tokio::dsn::{CredentialsFile, DEFAULT_BRANCH, DEFAULT_PORT, DEFAULT_USER};
use humantime::format_duration;
use std::io::IsTerminal;
use tokio::join;
use tokio::time::sleep;

use crate::connect::Connection;
use crate::options::{CloudOptions, InstanceOptionsLegacy};
use gel_tokio::{Builder, CloudName, InstanceName};

use crate::branding::{BRANDING_CLOUD, QUERY_TAG};
use crate::cloud;
use crate::cloud::client::CloudClient;
use crate::collect::Collector;
use crate::commands::ExitCode;
use crate::credentials;
use crate::format;
use crate::platform::data_dir;
use crate::portable::exit_codes;
use crate::portable::instance::control;
use crate::portable::instance::upgrade::{BackupMeta, UpgradeMeta};
use crate::portable::local::{InstanceInfo, Paths};
use crate::portable::local::{lock_file, read_ports};
use crate::portable::{linux, macos, windows};
use crate::print::{self, Highlight, msg};
use crate::process;
use crate::table::{self, Cell, Row, Table};

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct List {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Output more debug info about each instance.
    #[arg(long, conflicts_with_all=&["debug", "json"])]
    pub extended: bool,

    /// Output all available debug info about each instance.
    #[arg(long, hide = true)]
    #[arg(conflicts_with_all=&["extended", "json"])]
    pub debug: bool,

    /// Output in JSON format.
    #[arg(long, conflicts_with_all=&["extended", "debug"])]
    pub json: bool,

    /// Query remote instances.
    //  Currently needed for WSL.
    #[arg(long, hide = true)]
    pub no_remote: bool,

    /// Do not show warnings on no instances.
    //  Currently needed for WSL.
    #[arg(long, hide = true)]
    pub quiet: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Status {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    #[command(flatten)]
    pub instance_opts: InstanceOptionsLegacy,

    /// Show current systems service info.
    #[arg(long, conflicts_with_all=&["debug", "json", "extended"])]
    pub service: bool,

    /// Output more debug info about each instance.
    #[arg(long, conflicts_with_all=&["debug", "json", "service"])]
    pub extended: bool,

    /// Output all available debug info about each instance.
    #[arg(long, hide = true)]
    #[arg(conflicts_with_all=&["extended", "json", "service"])]
    pub debug: bool,

    /// Output in JSON format.
    #[arg(long, conflicts_with_all=&["extended", "debug", "service"])]
    pub json: bool,

    /// Do not print error on "No instance found", only indicate by error code.
    //  Currently needed for WSL.
    #[arg(long, hide = true)]
    pub quiet: bool,
}

#[derive(Debug)]
pub enum Service {
    Ready,
    Running { pid: u32 },
    Failed { exit_code: Option<u16> },
    Inactive { error: String },
}

#[derive(Debug)]
pub enum DataDirectory {
    Absent,
    NoMetadata,
    Upgrading(anyhow::Result<UpgradeMeta>),
    Normal,
}

#[derive(Debug)]
pub enum BackupStatus {
    Absent,
    Exists {
        backup_meta: anyhow::Result<BackupMeta>,
        data_meta: anyhow::Result<InstanceInfo>,
    },
}

#[derive(Debug)]
pub struct FullStatus {
    pub name: String,
    pub service: Service,
    pub instance: anyhow::Result<InstanceInfo>,
    pub reserved_port: Option<u16>,
    pub data_dir: PathBuf,
    pub data_status: DataDirectory,
    pub backup: BackupStatus,
    pub credentials_file_exists: bool,
    pub service_exists: bool,
    // TODO(tailhook) add linked projects
}

#[derive(Debug)]
pub enum ConnectionStatus {
    Connected,
    Refused,
    TimedOut,
    Error(anyhow::Error),
}

#[derive(Debug)]
pub enum RemoteType {
    Remote,
    Cloud { instance_id: String },
}

#[derive(Debug)]
pub struct RemoteStatus {
    pub name: String,
    pub type_: RemoteType,
    pub credentials: CredentialsFile,
    pub version: Option<String>,
    pub connection: Option<ConnectionStatus>,
    pub instance_status: Option<String>,
    pub location: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct JsonStatus {
    pub name: String,
    pub port: Option<u16>,
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_instance_id: Option<String>,
}

pub fn run(cmd: &Status, opts: &crate::options::Options) -> anyhow::Result<()> {
    if cmd.service {
        external_status(cmd)
    } else {
        normal_status(cmd, opts)
    }
}

fn external_status(options: &Status) -> anyhow::Result<()> {
    let name = match options.instance_opts.instance()? {
        InstanceName::Local(name) => name,
        InstanceName::Cloud { .. } => todo!(),
    };
    let meta = &(InstanceInfo::read(&name)?);
    if cfg!(windows) {
        windows::external_status(meta)
    } else if cfg!(target_os = "macos") {
        macos::external_status(meta)
    } else if cfg!(target_os = "linux") {
        linux::external_status(meta)
    } else {
        anyhow::bail!("unsupported platform");
    }
}

fn is_run_by_supervisor(name: &str) -> anyhow::Result<Option<bool>> {
    let lock_path = lock_file(name)?;
    match fs::read_to_string(&lock_path) {
        Ok(s) if s == "systemd" && cfg!(target_os = "linux") => Ok(Some(true)),
        Ok(s) if s == "launchctl" && cfg!(target_os = "macos") => Ok(Some(true)),
        Ok(_) => Ok(Some(false)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context(format!("cannot read {lock_path:?}"))?,
    }
}

fn service_status(name: &str) -> anyhow::Result<Service> {
    let run_by_super = is_run_by_supervisor(name)?;
    let mut pid = None;
    if !run_by_super.unwrap_or(false) {
        if let Some(file_pid) = control::read_pid(name)? {
            if process::exists(file_pid) {
                pid = Some(file_pid);
            }
        }
    };

    let service = if let Some(pid) = pid {
        Service::Running { pid }
    } else if control::detect_supervisor(name) {
        if cfg!(windows) {
            windows::service_status(name)
        } else if cfg!(target_os = "macos") {
            macos::service_status(name)
        } else if cfg!(target_os = "linux") {
            linux::service_status(name)
        } else {
            anyhow::bail!("unsupported platform")
        }
    } else {
        anyhow::bail!("no supervisor found and no active pid exists");
    };
    Ok(service)
}

fn status_from_meta(
    name: &str,
    paths: &Paths,
    instance: anyhow::Result<InstanceInfo>,
) -> FullStatus {
    let service = service_status(name).unwrap_or_else(|e| Service::Inactive {
        error: e.to_string(),
    });
    let reserved_port = read_ports().ok().and_then(|map| map.get(name).cloned());
    let data_status = if paths.data_dir.exists() {
        if paths.upgrade_marker.exists() {
            DataDirectory::Upgrading(read_upgrade(&paths.upgrade_marker))
        } else if instance.is_ok() {
            DataDirectory::Normal
        } else {
            DataDirectory::NoMetadata
        }
    } else {
        DataDirectory::Absent
    };
    let backup = backup_status(name, &paths.backup_dir);
    let credentials_file_exists = paths.credentials.exists();
    let service_exists = paths.service_files.iter().any(|f| f.exists());
    FullStatus {
        name: name.into(),
        service,
        instance,
        reserved_port,
        data_dir: paths.data_dir.clone(),
        data_status,
        backup,
        credentials_file_exists,
        service_exists,
    }
}

pub fn instance_status(name: &str) -> anyhow::Result<FullStatus> {
    let paths = Paths::get(name)?; // the only error case
    let meta = InstanceInfo::read(name);
    Ok(status_from_meta(name, &paths, meta))
}

fn normal_status(cmd: &Status, opts: &crate::options::Options) -> anyhow::Result<()> {
    let name = match cmd.instance_opts.instance()? {
        InstanceName::Local(name) => name,
        InstanceName::Cloud(name) => {
            return cloud_status(cmd, &name, opts);
        }
    };
    let meta = InstanceInfo::try_read(&name).transpose();
    if let Some(meta) = meta {
        let paths = Paths::get(&name)?;
        let status = status_from_meta(&name, &paths, meta);
        if cmd.debug {
            println!("{status:#?}");
            Ok(())
        } else if cmd.extended {
            status.print_extended_and_exit();
        } else if cmd.json {
            status.print_json_and_exit();
        } else {
            status.print_and_exit();
        }
    } else {
        remote_status(cmd)
    }
}

fn cloud_status(
    cmd: &Status,
    name: &CloudName,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    let client = CloudClient::new(&opts.cloud_options)?;
    client.ensure_authenticated()?;

    let inst = cloud::ops::find_cloud_instance_by_name(name, &client)?
        .ok_or_else(|| anyhow::anyhow!("instance not found"))?;

    let status = cloud::ops::get_status(&client, &inst)?;

    if cmd.extended {
        status.print_extended_and_exit();
    } else if cmd.json {
        status.print_json_and_exit();
    } else {
        status.print_and_exit();
    }
}

async fn try_get_version(creds: &CredentialsFile) -> anyhow::Result<String> {
    let config = Builder::new()
        .params(creds.clone())
        .without_system()
        .build()?;
    let mut conn = Connection::connect(&config, QUERY_TAG).await?;
    let ver = conn
        .query_required_single("SELECT sys::get_version_as_str()", &())
        .await
        .context("cannot fetch database version")?;
    Ok(ver)
}

pub async fn try_connect(creds: &CredentialsFile) -> (Option<String>, ConnectionStatus) {
    use tokio::time::timeout;
    match timeout(Duration::from_secs(2), try_get_version(creds)).await {
        Ok(Ok(ver)) => (Some(ver), ConnectionStatus::Connected),
        Ok(Err(e)) => {
            let inner = e.source().and_then(|e| e.downcast_ref::<io::Error>());
            if let Some(e) = inner {
                if e.kind() == io::ErrorKind::ConnectionRefused {
                    return (None, ConnectionStatus::Refused);
                }
            }
            (None, ConnectionStatus::Error(e))
        }
        Err(_) => (None, ConnectionStatus::TimedOut),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn remote_status_with_feedback(name: &str, quiet: bool) -> anyhow::Result<RemoteStatus> {
    intermediate_feedback(_remote_status(name, quiet), || "Trying to connect...").await
}

async fn _remote_status(name: &str, quiet: bool) -> anyhow::Result<RemoteStatus> {
    let cred_path = credentials::path(name)?;
    if !cred_path.exists() {
        if !quiet {
            msg!(
                "{} No instance {} found",
                print::err_marker(),
                name.emphasized()
            );
        }
        return Err(ExitCode::new(exit_codes::INSTANCE_NOT_FOUND).into());
    }
    let cred_data = tokio::fs::read(cred_path).await?;
    let credentials = serde_json::from_slice(&cred_data)?;
    let (version, connection) = try_connect(&credentials).await;
    let location = format!(
        "{}:{}",
        credentials.host.as_deref().unwrap_or("localhost"),
        credentials.port.map(NonZero::get).unwrap_or(DEFAULT_PORT)
    );
    Ok(RemoteStatus {
        name: name.into(),
        type_: RemoteType::Remote,
        credentials,
        version,
        connection: Some(connection),
        instance_status: None,
        location,
    })
}

async fn intermediate_feedback<F, D>(future: F, text: impl FnOnce() -> D) -> F::Output
where
    F: Future,
    D: Display,
{
    tokio::select!(
        r = future => r,
        _ = async {
            sleep(Duration::from_millis(300)).await;
            if std::io::stderr().is_terminal() {
                eprintln!("{}", text());
            }
            pending().await
        } => unreachable!(),
    )
}

pub fn remote_status(options: &Status) -> anyhow::Result<()> {
    let name = match options.instance_opts.instance()? {
        InstanceName::Local(name) => name,
        InstanceName::Cloud { .. } => unreachable!("remote_status got cloud instance"),
    };

    let status = remote_status_with_feedback(&name, options.quiet)?;
    if options.service {
        println!("Remote instance");
    } else if options.debug {
        println!("{status:#?}");
    } else if options.extended {
        status.print_extended();
    } else if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status.json()).expect("status is json-serializable"),
        );
    } else if let Some(inst_status) = &status.instance_status {
        println!("{inst_status}");
    } else if let Some(ConnectionStatus::Error(e)) = &status.connection {
        print::error!("{e}");
    } else if let Some(conn_status) = &status.connection {
        println!("{}", conn_status.as_str());
    } else {
        println!("unknown");
    }
    status.exit()
}

pub fn list_local(
    dir: &Path,
) -> anyhow::Result<impl Iterator<Item = anyhow::Result<(String, PathBuf)>> + '_> {
    let err_ctx = move || format!("error reading directory {dir:?}");
    let dir = fs::read_dir(dir).with_context(err_ctx)?;
    Ok(dir.filter_map(move |result| {
        let entry = match result {
            Ok(entry) => entry,
            res => return Some(Err(res.with_context(err_ctx).unwrap_err())),
        };
        let fname = entry.file_name();
        let name_op = fname
            .to_str()
            .and_then(|x| InstanceName::from_str(x).is_ok().then_some(x));
        if let Some(name) = name_op {
            Some(Ok((name.into(), entry.path())))
        } else {
            log::info!("Skipping directory {:?}", entry.path());
            None
        }
    }))
}

async fn get_remote_async(
    instances: Vec<String>,
    errors: &Collector<anyhow::Error>,
) -> anyhow::Result<Vec<RemoteStatus>> {
    let sem = Arc::new(tokio::sync::Semaphore::new(100));
    let mut tasks = tokio::task::JoinSet::new();
    for name in instances {
        let errors = errors.sender();
        let permit = sem.clone().acquire_owned().await.expect("semaphore is ok");
        tasks.spawn(async move {
            let _permit = permit;
            match _remote_status(&name, false).await {
                Ok(status) => {
                    if let Some(ConnectionStatus::Error(e)) = &status.connection {
                        errors.add(
                            // Can't use `e.context()` because can't clone
                            // the error
                            anyhow::anyhow!("probing {:?}: {:#}", name, e),
                        );
                    }
                    Some(status)
                }
                Err(e) => {
                    errors.add(e.context(format!("probing {name:?}")));
                    None
                }
            }
        });
    }
    let mut result = Vec::new();
    while let Some(res) = tasks.join_next().await {
        if let Some(status) = res? {
            result.push(status);
        }
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

async fn get_remote_and_cloud(
    instances: Vec<String>,
    cloud_client: CloudClient,
    errors: &Collector<anyhow::Error>,
) -> anyhow::Result<Vec<RemoteStatus>> {
    match join!(
        get_remote_async(instances, errors),
        crate::cloud::ops::list(cloud_client, errors),
    ) {
        (Ok(remote), Ok(cloud)) => Ok(remote.into_iter().chain(cloud.into_iter()).collect()),
        (Ok(remote), Err(e)) => {
            errors.add(e);
            Ok(remote)
        }
        (Err(e), Ok(cloud)) => {
            errors.add(e);
            Ok(cloud)
        }
        (Err(er), Err(ec)) => {
            errors.add(ec);
            Err(er)
        }
    }
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_remote(
    visited: &BTreeSet<String>,
    opts: &crate::options::Options,
    errors: &Collector<anyhow::Error>,
) -> anyhow::Result<Vec<RemoteStatus>> {
    _get_remote(visited, opts, errors).await
}

async fn _get_remote(
    visited: &BTreeSet<String>,
    opts: &crate::options::Options,
    errors: &Collector<anyhow::Error>,
) -> anyhow::Result<Vec<RemoteStatus>> {
    let cloud_client = CloudClient::new(&opts.cloud_options)?;
    let instances: Vec<_> = credentials::all_instance_names()?
        .into_iter()
        .filter(|name| !visited.contains(name))
        .collect();
    let num = instances.len();
    if cloud_client.is_logged_in {
        intermediate_feedback(
            get_remote_and_cloud(instances, cloud_client, errors),
            || {
                if num > 0 {
                    format!("Checking {BRANDING_CLOUD} and {num} remote instance(s)...")
                } else {
                    format!("Checking {BRANDING_CLOUD} instances...")
                }
            },
        )
        .await
    } else if num > 0 {
        intermediate_feedback(get_remote_async(instances, errors), || {
            format!("Checking {num} remote instance(s)...")
        })
        .await
    } else {
        Ok(Vec::new())
    }
}

fn list_local_status(visited: &mut BTreeSet<String>) -> anyhow::Result<Vec<FullStatus>> {
    let mut local = Vec::new();
    let data_dir = data_dir()?;
    if data_dir.exists() {
        for pair in list_local(&data_dir)? {
            let (name, path) = pair?;
            if path.join("metadata.json").exists() {
                // consider deprecated instances remote,
                // i.e. not adding them in "visited"
                log::debug!(
                    "Instance {:?} has deprecated install method. \
                            Skipping.",
                    name
                );
            } else {
                visited.insert(name.clone());
                local.push(instance_status(&name)?);
            }
        }
        local.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(local)
}

pub fn list(options: &List, opts: &crate::options::Options) -> anyhow::Result<()> {
    let errors = Collector::new();
    let mut visited = BTreeSet::new();
    let local = match list_local_status(&mut visited) {
        Ok(local) => local,
        Err(e) => {
            errors.add(e);
            Vec::new()
        }
    };

    let remote = if options.no_remote {
        Vec::new()
    } else {
        match get_remote(&visited, opts, &errors) {
            Ok(remote) => remote,
            Err(e) => {
                errors.add(e);
                Vec::new()
            }
        }
    };

    if local.is_empty() && remote.is_empty() {
        return if print_errors(&errors.list(), false) {
            Err(ExitCode::new(1).into())
        } else {
            if options.json {
                println!("[]");
            } else if !options.quiet {
                print::warn!("No instances found");
            }
            Ok(())
        };
    }
    if options.debug {
        for status in local {
            println!("{status:#?}");
        }
        for status in remote {
            println!("{status:#?}");
        }
    } else if options.extended {
        for status in local {
            status.print_extended();
        }
        for status in remote {
            status.print_extended();
        }
    } else if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &local
                    .iter()
                    .map(|status| status.json())
                    .chain(remote.iter().map(|status| status.json()))
                    .collect::<Vec<_>>()
            )?
        );
    } else {
        // using always JSON because we need that for windows impl
        let local_json = local.iter().map(|s| s.json()).collect::<Vec<_>>();
        print_table(&local_json, &remote);
    }

    if print_errors(&errors.list(), true) {
        Err(ExitCode::new(exit_codes::PARTIAL_SUCCESS).into())
    } else {
        Ok(())
    }
}

pub fn print_errors(errs: &[anyhow::Error], is_warning: bool) -> bool {
    for e in errs {
        if is_warning {
            print::warn!("Warning: {e:#}");
        } else {
            print::error!("{e}");
        }
    }
    !errs.is_empty()
}

pub fn print_table(local: &[JsonStatus], remote: &[RemoteStatus]) {
    let mut table = Table::new();
    table.set_format(*table::FORMAT);
    table.set_titles(Row::new(
        ["Kind", "Name", "Location", "Version", "Status"]
            .iter()
            .map(|x| table::header_cell(x))
            .collect(),
    ));
    for status in local {
        table.add_row(Row::new(vec![
            Cell::new("local"),
            Cell::new(&status.name),
            Cell::new(&format!(
                "localhost:{}",
                status
                    .port
                    .as_ref()
                    .map(ToString::to_string)
                    .as_deref()
                    .unwrap_or("?")
            )),
            Cell::new(status.version.as_deref().unwrap_or("?")),
            Cell::new(status.service_status.as_deref().unwrap_or("?")),
        ]));
    }
    for status in remote {
        table.add_row(Row::new(vec![
            Cell::new(match status.type_ {
                RemoteType::Cloud { instance_id: _ } => "cloud",
                RemoteType::Remote => "remote",
            }),
            Cell::new(&status.name),
            Cell::new(&status.location),
            Cell::new(
                status
                    .version
                    .as_ref()
                    .map(|m| m.to_string())
                    .as_deref()
                    .unwrap_or("?"),
            ),
            Cell::new(
                status
                    .instance_status
                    .as_deref()
                    .or(status.connection.as_ref().map(|s| s.as_str()))
                    .unwrap_or("unknown"),
            ),
        ]));
    }
    table.printstd();
}

impl FullStatus {
    pub fn print_extended_and_exit(&self) -> ! {
        self.print_extended();
        self.exit()
    }
    fn print_extended(&self) {
        println!("{}:", self.name);

        print!("  Status: ");
        match &self.service {
            Service::Ready => {
                println!("ready in socket activation mode, not running");
            }
            Service::Running { pid } => {
                println!("running, pid {pid}");
                println!("  Pid: {pid}");
            }
            Service::Failed {
                exit_code: Some(code),
            } => {
                println!("stopped, exit code {code}");
            }
            Service::Failed { exit_code: None } => {
                println!("not running");
            }
            Service::Inactive { error } => {
                println!("inactive");
                println!("  Inactivity assumed because: {error}");
            }
        }
        println!(
            "  Service/Container: {}",
            match self.service_exists {
                true => "exists",
                false => "NOT FOUND",
            }
        );
        println!(
            "  Credentials: {}",
            match self.credentials_file_exists {
                true => "exists",
                false => "NOT FOUND",
            }
        );

        match &self.instance {
            Ok(inst) => {
                if let Ok(version) = inst.get_version() {
                    println!("  Version: {version}");
                }
                if let Some(port) = self.reserved_port {
                    if inst.port == port {
                        println!("  Port: {port}");
                    } else {
                        println!("  Port: {} (but {} reserved)", inst.port, port);
                    }
                } else {
                    println!("  Port: {}", inst.port);
                }
            }
            _ => {
                if let Some(port) = self.reserved_port {
                    println!("  Port: {port} (reserved)");
                }
            }
        }

        println!("  Data directory: {}", self.data_dir.display());
        println!(
            "  Data status: {}",
            match &self.data_status {
                DataDirectory::Absent => "NOT FOUND".into(),
                DataDirectory::NoMetadata => "METADATA ERROR".into(),
                DataDirectory::Upgrading(Err(e)) => format!("upgrading ({e:#})"),
                DataDirectory::Upgrading(Ok(up)) => {
                    format!(
                        "upgrading {} -> {} for {}",
                        up.source,
                        up.target,
                        format_duration(up.started.elapsed().unwrap_or(Duration::new(0, 0)))
                    )
                }
                DataDirectory::Normal => "normal".into(),
            }
        );
        println!(
            "  Backup: {}",
            match &self.backup {
                BackupStatus::Absent => "absent".into(),
                BackupStatus::Exists {
                    backup_meta: Err(e),
                    ..
                } => {
                    format!("present (error: {e:#})")
                }
                BackupStatus::Exists {
                    backup_meta: Ok(b), ..
                } => {
                    format!("present, {}", format::done_before(b.timestamp))
                }
            }
        );
    }
    pub fn json(&self) -> JsonStatus {
        let meta = self.instance.as_ref().ok();
        JsonStatus {
            name: self.name.clone(),
            port: meta.map(|m| m.port),
            version: meta
                .and_then(|m| m.get_version().ok())
                .map(|v| v.to_string()),
            service_status: Some(status_str(&self.service).to_string()),
            remote_status: None,
            instance_status: None,
            cloud_instance_id: None,
        }
    }
    pub fn print_json_and_exit(&self) -> ! {
        println!(
            "{}",
            serde_json::to_string_pretty(&self.json()).expect("status is not json-serializable")
        );
        self.exit()
    }
    pub fn print_and_exit(&self) -> ! {
        use Service::*;
        match &self.service {
            Ready => {
                eprintln!("Ready in socket activation mode, not running");
            }
            Running { pid } => {
                eprint!("Running, pid ");
                println!("{pid}");
            }
            Failed {
                exit_code: Some(code),
            } => {
                eprintln!("Stopped, exit code {code}");
            }
            Failed { exit_code: None } => {
                eprintln!("Not running");
            }
            Inactive { .. } => {
                eprintln!("Inactive");
            }
        }
        // TODO(tailhook) print more information in case some error is found:
        // Socket is occupied, while not running
        // No service file or no data directory
        // ..etc.
        self.exit()
    }
    fn exit(&self) -> ! {
        use Service::*;

        match self.service {
            Ready => exit(0),
            Running { .. } => exit(0),
            Failed { .. } => exit(3),
            Inactive { .. } => exit(3),
        }
    }
}

impl RemoteStatus {
    pub fn print_extended(&self) {
        println!("{}:", self.name);
        let is_cloud = if let RemoteType::Cloud { instance_id } = &self.type_ {
            println!("  {BRANDING_CLOUD} Instance ID: {instance_id}");
            true
        } else {
            false
        };
        if let Some(conn_status) = &self.connection {
            println!("  Connection status: {}", conn_status.as_str());
        }
        if let Some(inst_status) = &self.instance_status {
            println!("  Instance status: {inst_status}");
        }
        if !is_cloud {
            println!("  Credentials: exist");
        }
        println!(
            "  Version: {}",
            self.version.as_ref().map_or("unknown", |x| &x[..])
        );
        let creds = &self.credentials;
        println!(
            "  Host: {}",
            creds.host.as_ref().map_or("localhost", |x| &x[..])
        );
        println!(
            "  Port: {}",
            creds.port.map(NonZero::get).unwrap_or(DEFAULT_PORT)
        );
        if !is_cloud {
            println!("  User: {}", creds.user.as_deref().unwrap_or(DEFAULT_USER));
            if let Some(db) = creds.database.as_ref() {
                println!("  Database: {db}");
            } else if let Some(branch) = creds.branch.as_ref() {
                println!("  Branch: {branch}");
            } else {
                println!("  Branch: (default)");
            }
        }
        if let Some(ConnectionStatus::Error(e)) = &self.connection {
            println!("  Connection error: {e:#}");
        }
    }

    pub fn print_extended_and_exit(&self) -> ! {
        self.print_extended();
        self.exit()
    }

    pub fn json(&self) -> JsonStatus {
        JsonStatus {
            name: self.name.clone(),
            port: self.credentials.port.map(NonZero::get),
            version: self.version.clone(),
            service_status: None,
            remote_status: self.connection.as_ref().map(|s| s.as_str().to_string()),
            instance_status: self.instance_status.clone(),
            cloud_instance_id: if let RemoteType::Cloud { instance_id } = &self.type_ {
                Some(instance_id.clone())
            } else {
                None
            },
        }
    }

    pub fn print_json_and_exit(&self) -> ! {
        println!(
            "{}",
            serde_json::to_string_pretty(&self.json()).expect("status is not json-serializable")
        );
        self.exit()
    }

    pub fn print_and_exit(&self) -> ! {
        eprintln!("{}", self.instance_status.as_deref().unwrap_or("<unknown>"));
        self.exit()
    }

    pub fn exit(&self) -> ! {
        match &self.connection {
            Some(ConnectionStatus::Connected) => exit(0),
            Some(_) => exit(3),
            None => match &self.instance_status {
                Some(_) => exit(0),
                None => exit(4),
            },
        }
    }
}

impl ConnectionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnectionStatus::Connected => "up",
            ConnectionStatus::Refused => "refused",
            ConnectionStatus::TimedOut => "timed out",
            ConnectionStatus::Error(..) => "error",
        }
    }
}

fn status_str(status: &Service) -> &'static str {
    match status {
        Service::Ready => "ready",
        Service::Running { .. } => "running",
        Service::Failed { .. } => "not running",
        Service::Inactive { .. } => "inactive",
    }
}

pub fn backup_status(name: &str, dir: &Path) -> BackupStatus {
    use BackupStatus::*;
    if !dir.exists() {
        return Absent;
    }
    let bmeta_json = dir.join("backup.json");
    let backup_meta = fs::read(&bmeta_json)
        .with_context(|| format!("error reading {}", bmeta_json.display()))
        .and_then(|data| {
            serde_json::from_slice(&data)
                .with_context(|| format!("error decoding {}", bmeta_json.display()))
        });
    let dmeta_json = dir.join("instance_info.json");
    let data_meta = InstanceInfo::read_at(name, &dmeta_json);
    Exists {
        backup_meta,
        data_meta,
    }
}

#[context("failed to read upgrade marker {:?}", file)]
pub fn read_upgrade(file: &Path) -> anyhow::Result<UpgradeMeta> {
    Ok(serde_json::from_slice(&fs::read(file)?)?)
}
