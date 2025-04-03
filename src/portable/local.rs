use std::collections::btree_set;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::iter::Peekable;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context;
use fn_error_context::context;

use gel_dsn::gel::UnixPath;
use gel_tokio::{Builder, Config, InstanceName};

use crate::branding::BRANDING;
use crate::bug;
use crate::credentials;
use crate::hint::HintExt;
use crate::platform::{cache_dir, config_dir, data_dir, portable_dir};
use crate::portable::repository::PackageHash;
use crate::portable::ver;
use crate::portable::{linux, macos, windows};

const MIN_PORT: u16 = 10700;

#[derive(Debug)]
pub struct Paths {
    pub credentials: PathBuf,
    pub data_dir: PathBuf,
    pub service_files: Vec<PathBuf>,
    pub dump_path: PathBuf,
    pub backup_dir: PathBuf,
    pub upgrade_marker: PathBuf,
    pub runstate_dir: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstanceInfo {
    #[serde(skip)]
    pub name: String,
    #[serde(skip)]
    pub instance_name: InstanceName,
    pub installation: Option<InstallInfo>,
    pub port: u16,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PartialInstanceInfo {
    pub installation: Option<InstallInfo>,
    pub port: u16,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstallInfo {
    pub version: ver::Build,
    pub package_url: url::Url,
    pub package_hash: PackageHash,
    #[serde(with = "serde_millis")]
    pub installed_at: SystemTime,
    #[serde(default)]
    pub slot: String,
}

fn port_file() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("instance_ports.json"))
}

pub fn log_file(instance: &str) -> anyhow::Result<PathBuf> {
    Ok(cache_dir()?.join(format!("logs/{instance}.log")))
}

pub fn lock_file(instance: &str) -> anyhow::Result<PathBuf> {
    Ok(runstate_dir(instance)?.join("service.lock"))
}

pub fn open_lock(instance: &str) -> anyhow::Result<fd_lock::RwLock<fs::File>> {
    let lock_path = lock_file(instance)?;
    if let Some(parent) = lock_path.parent() {
        fs_err::create_dir_all(parent)?;
    }
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(&lock_path)
        .with_context(|| format!("cannot open lock file {lock_path:?}"))?;
    Ok(fd_lock::RwLock::new(lock_file))
}

pub fn runstate_dir(instance: &str) -> anyhow::Result<PathBuf> {
    if cfg!(target_os = "linux") {
        if let Some(dir) = dirs::runtime_dir() {
            return Ok(dir.join(format!("edgedb-{instance}")));
        }
    }
    Ok(cache_dir()?.join("run").join(instance))
}

pub fn read_ports() -> anyhow::Result<BTreeMap<String, u16>> {
    _read_ports(&port_file()?)
}

#[context("failed reading port mapping {}", path.display())]
fn _read_ports(path: &Path) -> anyhow::Result<BTreeMap<String, u16>> {
    let data = match fs::read_to_string(path) {
        Ok(data) if data.is_empty() => {
            return Ok(BTreeMap::new());
        }
        Ok(data) => data,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(e) => return Err(e)?,
    };
    Ok(serde_json::from_str(&data)?)
}

struct NextMinPort {
    reserved: Peekable<btree_set::IntoIter<u16>>,
    prev: u16,
}

impl NextMinPort {
    fn search(port_map: &BTreeMap<String, u16>) -> NextMinPort {
        NextMinPort {
            reserved: port_map
                .values()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .peekable(),
            prev: MIN_PORT - 1,
        }
    }
}

impl Iterator for NextMinPort {
    type Item = u16;
    fn next(&mut self) -> Option<u16> {
        loop {
            if let Some(&next) = self.reserved.peek() {
                if next > self.prev + 1 {
                    self.prev += 1;
                    return Some(self.prev);
                } else {
                    self.reserved.next();
                    self.prev = next;
                    continue;
                }
            } else {
                let result = self.prev.checked_add(1);
                self.prev = self.prev.saturating_add(1);
                return result;
            }
        }
    }
}

pub fn allocate_port(name: &str) -> anyhow::Result<u16> {
    let port_file = port_file()?;
    let mut port_map = _read_ports(&port_file)?;
    if let Some(port) = port_map.get(name) {
        return Ok(*port);
    }
    for port in NextMinPort::search(&port_map) {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                log::debug!("Address 127.0.0.1:{} is already in use", port);
                continue;
            }
            Err(e) => {
                log::warn!("Error checking port 127.0.0.1:{}: {:#}", port, e);
            }
        }
        match TcpListener::bind(("::1", port)) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                log::debug!("Address [::1]:{} is already in use", port);
                continue;
            }
            Err(e) => {
                log::warn!("Error checking port [::1]:{}: {:#}", port, e);
            }
        }
        port_map.insert(name.to_string(), port);
        write_json(&port_file, "ports mapping", &port_map)?;
        return Ok(port);
    }
    anyhow::bail!("Cannot find unused port");
}

#[context("cannot write {} file {}", title, path.display())]
pub fn write_json<T: serde::Serialize>(path: &Path, title: &str, data: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let f = io::BufWriter::new(fs::File::create(path)?);
    serde_json::to_writer_pretty(f, data)?;
    Ok(())
}

fn list_installed(
    dir: &Path,
) -> anyhow::Result<impl Iterator<Item = anyhow::Result<(ver::Specific, PathBuf)>> + '_> {
    let err_ctx = move || format!("error reading directory {dir:?}");
    let dir = fs::read_dir(dir).with_context(err_ctx)?;
    Ok(dir.filter_map(move |result| {
        let entry = match result {
            Ok(entry) => entry,
            res => return Some(Err(res.with_context(err_ctx).unwrap_err())),
        };
        let ver_opt = entry.file_name().to_str().and_then(|x| x.parse().ok());
        if let Some(ver) = ver_opt {
            Some(Ok((ver, entry.path())))
        } else {
            log::info!("Skipping directory {:?}", entry.path());
            None
        }
    }))
}

#[context("cannot read {:?}", path)]
fn _read_install_info(path: &Path) -> anyhow::Result<InstallInfo> {
    let file = fs::File::open(path)?;
    let file = io::BufReader::new(file);
    Ok(serde_json::from_reader(file)?)
}

impl InstallInfo {
    pub fn read(dir: &Path) -> anyhow::Result<InstallInfo> {
        _read_install_info(&dir.join("install_info.json"))
    }
}

#[context("failed to list installed packages")]
pub fn get_installed() -> anyhow::Result<Vec<InstallInfo>> {
    let mut installed = Vec::with_capacity(8);
    let dir = portable_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    for result in list_installed(&dir)? {
        let (ver, path) = result?;
        match InstallInfo::read(&path) {
            Ok(info) if ver != info.version.specific() => {
                log::warn!(
                    "Mismatching package version in {:?}: {} != {}",
                    path,
                    info.version,
                    ver
                );
                continue;
            }
            Ok(info) => installed.push(info),
            Err(e) => log::warn!("Skipping {:?}: {:#}", path, e),
        }
    }
    Ok(installed)
}

pub fn instance_data_dir(name: &str) -> anyhow::Result<PathBuf> {
    if cfg!(windows) {
        Err(bug::error("Data dir is not used for instances on Windows"))
    } else {
        Ok(data_dir()?.join(name))
    }
}

impl Paths {
    pub fn get(name: &str) -> anyhow::Result<Paths> {
        let base = data_dir()?;
        Ok(Paths {
            credentials: credentials::path(name)?,
            data_dir: base.join(name),
            dump_path: base.join(format!("{name}.dump")),
            backup_dir: base.join(format!("{name}.backup")),
            upgrade_marker: base.join(format!("{name}.UPGRADE_IN_PROGRESS")),
            runstate_dir: runstate_dir(name)?,
            service_files: if cfg!(windows) {
                windows::service_files(name)?
            } else if cfg!(target_os = "macos") {
                macos::service_files(name)?
            } else if cfg!(target_os = "linux") {
                linux::service_files(name)?
            } else {
                Vec::new()
            },
        })
    }
    pub fn check_exists(&self) -> anyhow::Result<()> {
        if self.credentials.exists() {
            anyhow::bail!("Credentials file {:?} already exists", self.credentials);
        }
        if self.data_dir.exists() {
            anyhow::bail!("Data directory {:?} already exists", self.data_dir);
        }
        for path in &self.service_files {
            if path.exists() {
                anyhow::bail!("Service file {:?} already exists", path);
            }
        }
        Ok(())
    }
}

impl InstanceInfo {
    pub fn get_version(&self) -> anyhow::Result<&ver::Build> {
        Ok(&self.get_installation()?.version)
    }

    pub fn try_read(name: &str) -> anyhow::Result<Option<InstanceInfo>> {
        if cfg!(windows) {
            let data = match windows::get_instance_info(name) {
                Ok(data) => data,
                Err(e) => {
                    // TODO(tailhook) better differentiate the error
                    log::info!("Reading instance info failed with {:#}", e);
                    return Ok(None);
                }
            };
            let data: PartialInstanceInfo = serde_json::from_str(&data)?;
            Ok(Some(InstanceInfo {
                name: name.into(),
                instance_name: InstanceName::Local(name.to_string()),
                installation: data.installation,
                port: data.port,
            }))
        } else {
            let mut path = instance_data_dir(name)?;
            path.push("instance_info.json");
            // TODO(tailhook) check existence of the directory
            // and crash on existence of the file.
            // But this can only be done, once we get rid of old install methods
            if !path.exists() {
                return Ok(None);
            }
            Ok(Some(InstanceInfo::read_at(name, &path)?))
        }
    }

    pub fn read(name: &str) -> anyhow::Result<InstanceInfo> {
        if cfg!(windows) {
            let data = windows::get_instance_info(name)?;
            let data: PartialInstanceInfo = serde_json::from_str(&data)?;
            Ok(InstanceInfo {
                name: name.into(),
                instance_name: InstanceName::Local(name.to_string()),
                installation: data.installation,
                port: data.port,
            })
        } else {
            InstanceInfo::read_at(name, &instance_data_dir(name)?.join("instance_info.json"))
        }
    }

    #[context("error reading instance info: {:?}", path)]
    pub fn read_at(name: &str, path: &PathBuf) -> anyhow::Result<InstanceInfo> {
        let f = io::BufReader::new(fs::File::open(path)?);
        let data: PartialInstanceInfo = serde_json::from_reader(f)?;
        Ok(InstanceInfo {
            name: name.into(),
            instance_name: InstanceName::Local(name.to_string()),
            installation: data.installation,
            port: data.port,
        })
    }

    pub fn data_dir(&self) -> anyhow::Result<PathBuf> {
        instance_data_dir(&self.name)
    }

    fn get_installation(&self) -> anyhow::Result<&InstallInfo> {
        self.installation
            .as_ref()
            .ok_or_else(|| bug::error("installation should be set"))
    }

    pub fn server_path(&self) -> anyhow::Result<PathBuf> {
        self.get_installation()?.server_path()
    }

    #[allow(unused)]
    pub fn base_path(&self) -> anyhow::Result<PathBuf> {
        self.get_installation()?.base_path()
    }

    #[allow(unused)]
    pub fn extension_path(&self) -> anyhow::Result<PathBuf> {
        self.get_installation()?.extension_path()
    }

    pub fn extension_loader_path(&self) -> anyhow::Result<PathBuf> {
        self.get_installation()?.extension_loader_path()
    }

    pub fn admin_conn_params(&self) -> anyhow::Result<Config> {
        let config = Builder::new()
            .port(self.port)
            .unix_path(UnixPath::with_port_suffix(
                runstate_dir(&self.name)?.join(".s.EDGEDB.admin."),
            ))
            .wait_until_available(std::time::Duration::from_secs(300))
            .without_system()
            .build()?;
        Ok(config)
    }
}

fn installation_path(ver: &ver::Specific) -> anyhow::Result<PathBuf> {
    Ok(portable_dir()?.join(ver.to_string()))
}

impl InstallInfo {
    pub fn base_path(&self) -> anyhow::Result<PathBuf> {
        installation_path(&self.version.specific())
    }

    pub fn server_path(&self) -> anyhow::Result<PathBuf> {
        Ok(self.base_path()?.join("bin").join("edgedb-server"))
    }

    pub fn extension_path(&self) -> anyhow::Result<PathBuf> {
        let path = self
            .base_path()?
            .join("share")
            .join("data")
            .join("extensions");
        if !path.exists() {
            Err(
                bug::error("no extension directory available for this server")
                    .with_hint(|| {
                        format!(
                            "Extension installation requires {BRANDING} server version 6 or later"
                        )
                    })
                    .into(),
            )
        } else {
            Ok(path)
        }
    }

    pub fn extension_loader_path(&self) -> anyhow::Result<PathBuf> {
        let path = self.base_path()?.join("bin").join("edgedb-load-ext");
        if path.exists() {
            Ok(path)
        } else {
            Err(
                anyhow::anyhow!("edgedb-load-ext not found in the installation")
                    .with_hint(|| {
                        format!(
                            "Extension installation requires {BRANDING} server version 6 or later"
                        )
                    })
                    .into(),
            )
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Not a local instance")]
pub struct NonLocalInstance;

#[test]
fn test_min_port() {
    assert_eq!(
        NextMinPort::search(
            &vec![("a".into(), 10700), ("b".into(), 10702)]
                .into_iter()
                .collect()
        )
        .take(3)
        .collect::<Vec<_>>(),
        vec![10701, 10703, 10704],
    );
}
