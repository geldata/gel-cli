#![cfg_attr(not(windows), allow(unused_imports, dead_code))]

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{LazyLock, Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use const_format::formatcp;
use fn_error_context::context;
use gel_tokio::InstanceName;
use gel_tokio::dsn::CredentialsFile;
use log::warn;
use tempfile::tempfile;
use url::Url;

use crate::async_util;
use crate::branding::{BRANDING, BRANDING_CLI, BRANDING_CLI_CMD, BRANDING_WSL};
use crate::bug;
use crate::cli::env::Env;
use crate::cli::upgrade::{self, self_version};
use crate::collect::Collector;
use crate::commands::ExitCode;
use crate::credentials;
use crate::hint::HintExt;
use crate::instance;
use crate::instance::control;
use crate::instance::create;
use crate::instance::destroy;
use crate::instance::status::{self, status_str};
use crate::platform::{cache_dir, config_dir, tmp_file_path, wsl_dir};
use crate::portable::exit_codes;
use crate::portable::local::{InstanceInfo, NonLocalInstance, Paths, write_json};
use crate::portable::options;
use crate::portable::repository::{self, PackageHash, PackageInfo, download};
use crate::portable::server;
use crate::portable::ver;
use crate::print::{self, Highlight, msg};
use crate::process;
use crate::project;

use super::extension;

const CURRENT_DISTRO: &str = BRANDING_WSL;
static DISTRO_URL: LazyLock<Url> = LazyLock::new(|| {
    "https://aka.ms/wsl-debian-gnulinux"
        .parse()
        .expect("wsl url parsed")
});
const CERT_UPDATE_INTERVAL: Duration = Duration::from_secs(30 * 86400);

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
enum WslState {
    NotWsl,
    Wsl1,
    Wsl2,
}

static IS_IN_WSL: LazyLock<WslState> = LazyLock::new(|| {
    if cfg!(target_os = "linux") {
        let version = fs::read_to_string("/proc/version").unwrap_or_default();

        // Hint 1: /proc/version contains "microsoft" or "Microsoft"
        let version_contains_microsoft = version.to_lowercase().contains("microsoft");

        // Hint 2: https://superuser.com/questions/1749781/how-can-i-check-if-the-environment-is-wsl-from-a-shell-script
        // `/proc/sys/fs/binfmt_misc/WSLInterop` exists
        let interop_exists =
            std::fs::exists("/proc/sys/fs/binfmt_misc/WSLInterop").unwrap_or(false);

        if !version_contains_microsoft && !interop_exists {
            return WslState::NotWsl;
        }

        // https://askubuntu.com/questions/1177729/wsl-am-i-running-version-1-or-version-2
        let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
        if cmdline == "BOOT_IMAGE=/kernel init=/init" {
            WslState::Wsl1
        } else if cmdline.contains(r#"initrd=\initrd.img"#) {
            WslState::Wsl2
        } else {
            warn!(
                "Unknown WSL version: /proc/cmdline={:?} /proc/version={:?}, please report this as a bug",
                cmdline, version
            );
            WslState::Wsl2
        }
    } else {
        WslState::NotWsl
    }
});

const USR_BIN_EXE: &str = const_format::concatcp!("/usr/bin/", BRANDING_CLI_CMD);

#[derive(clap::Args, Clone, Debug)]
pub struct InitWslCommand {}

pub fn init_wsl(_cmd: &InitWslCommand, _opts: &crate::Options) -> anyhow::Result<()> {
    ensure_wsl()?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("WSL distribution is not installed")]
pub struct NoDistribution;

struct WslInit {
    distribution: String,
}

#[derive(Clone)]
pub struct Wsl {
    distribution: String,
    #[cfg(windows)]
    #[allow(dead_code)]
    lib: &'static wslapi::Library,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct WslInfo {
    distribution: String,
    last_checked_version: Option<ver::Semver>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cli_timestamp: Option<SystemTime>,
    cli_version: ver::Semver,
    certs_timestamp: SystemTime,
}

impl Wsl {
    pub fn cli_exe(&self) -> process::Native {
        let mut pro = process::Native::new(BRANDING_CLI_CMD, BRANDING_CLI_CMD, "wsl");
        pro.arg("--user").arg("edgedb");
        pro.arg("--distribution").arg(&self.distribution);
        pro.arg("_EDGEDB_FROM_WINDOWS=1");
        if let Some(log_env) = env::var_os("RUST_LOG") {
            let mut pair = OsString::with_capacity("RUST_LOG=".len() + log_env.len());
            pair.push("RUST_LOG=");
            pair.push(log_env);
            pro.arg(pair);
        }
        pro.arg(USR_BIN_EXE);
        pro.no_proxy();
        pro
    }
    #[cfg(windows)]
    #[allow(dead_code)]
    fn copy_out(&self, src: impl AsRef<str>, destination: impl AsRef<Path>) -> anyhow::Result<()> {
        let dest = path_to_linux(destination.as_ref())?;
        let cmd = format!(
            "cp {} {}",
            shell_escape::unix::escape(src.as_ref().into()),
            shell_escape::unix::escape(dest.into())
        );

        let code = self.lib.launch_interactive(
            &self.distribution,
            &cmd,
            /* current_working_dir */ false,
        )?;
        if code != 0 {
            anyhow::bail!("WSL command {:?} exited with exit code: {}", cmd, code);
        }
        Ok(())
    }

    fn read_text_file(&self, linux_path: impl AsRef<Path>) -> anyhow::Result<String> {
        process::Native::new("read file", "wsl", "wsl")
            .arg("--user")
            .arg("edgedb")
            .arg("--distribution")
            .arg(&self.distribution)
            .arg("cat")
            .arg(linux_path.as_ref())
            .get_stdout_text()
    }

    fn check_path_exist(&self, linux_path: impl AsRef<Path>) -> bool {
        process::Native::new("ls file", "wsl", "wsl")
            .arg("--user")
            .arg("edgedb")
            .arg("--distribution")
            .arg(&self.distribution)
            .arg("ls")
            .arg(linux_path.as_ref())
            .run()
            .is_ok()
    }

    #[cfg(not(windows))]
    fn copy_out(
        &self,
        _src: impl AsRef<str>,
        _destination: impl AsRef<Path>,
    ) -> anyhow::Result<()> {
        unreachable!();
    }
}

#[cfg(windows)]
fn copy_in(
    wsl: &wslapi::Library,
    distro: &str,
    src: impl AsRef<Path>,
    destination: impl AsRef<str>,
) -> anyhow::Result<()> {
    let src = path_to_linux(src.as_ref())?;
    let cmd = format!(
        "cp {} {}",
        shell_escape::unix::escape(src.into()),
        shell_escape::unix::escape(destination.as_ref().into())
    );
    let code = wsl.launch_interactive(distro, &cmd, /* current_working_dir */ false)?;
    if code != 0 {
        anyhow::bail!("WSL command {:?} exited with exit code: {}", cmd, code);
    }
    Ok(())
}

fn credentials_linux(instance: &str) -> String {
    format!("/home/edgedb/.config/edgedb/credentials/{instance}.json")
}

/// Credentials may be updated by various operations.
fn sync_credentials(instance: &str) -> anyhow::Result<()> {
    let wsl = ensure_wsl()?;
    let credentials = wsl.read_text_file(credentials_linux(instance))?;
    let credentials = CredentialsFile::from_str(&credentials)?;
    credentials::write(&InstanceName::Local(instance.to_string()), &credentials)?;
    Ok(())
}

#[context("cannot convert to linux (WSL) path {:?}", path)]
pub fn path_to_linux(path: &Path) -> anyhow::Result<String> {
    use std::path::Component::*;
    use std::path::Prefix::*;
    if !path.is_absolute() {
        return Err(bug::error("path must be absolute"));
    }

    let mut result = String::with_capacity(path.to_str().map(|m| m.len()).unwrap_or(32) + 32);
    result.push_str("/mnt");
    for component in path.components() {
        match component {
            Prefix(pre) => match pre.kind() {
                VerbatimDisk(c) | Disk(c) if c.is_ascii_alphabetic() => {
                    result.push('/');
                    result.push((c as char).to_ascii_lowercase());
                }
                _ => anyhow::bail!("unsupported prefix {:?}", pre),
            },
            RootDir => {}
            CurDir => return Err(bug::error("current dir in canonical path")),
            ParentDir => return Err(bug::error("parent dir in canonical path")),
            Normal(s) => {
                result.push('/');
                result.push_str(s.to_str().context("invalid characters in path")?);
            }
        }
    }
    Ok(result)
}

#[context("cannot convert WSL path to windows path {:?}", path)]
pub fn path_to_windows(path: &Path) -> anyhow::Result<PathBuf> {
    use std::path::Component::*;
    use std::path::Prefix::*;

    let mut result = PathBuf::with_capacity(path.to_str().map(|m| m.len()).unwrap_or(32) + 32);
    result.push(r"\\WSL$\");
    result.push(CURRENT_DISTRO);
    for component in path.components() {
        match component {
            RootDir => {}
            Prefix(..) => return Err(bug::error("prefix in unix path")),
            CurDir => return Err(bug::error("current dir in canonical path")),
            ParentDir => return Err(bug::error("parent dir in canonical path")),
            Normal(s) => result.push(s),
        }
    }
    Ok(result)
}

pub fn create_instance(options: &create::Command, name: &str, port: u16) -> anyhow::Result<()> {
    let wsl = ensure_wsl()?;

    let inner_options = create::Command {
        name: Some(InstanceName::Local(name.to_string())),
        instance: None,
        port: Some(port),
        ..options.clone()
    };
    wsl.cli_exe()
        .arg("instance")
        .arg("create")
        .args(&inner_options)
        .run()?;

    sync_credentials(name)?;
    Ok(())
}

pub fn destroy(options: &destroy::Command, name: &str) -> anyhow::Result<bool> {
    let mut found = false;
    if let Some(wsl) = get_wsl()? {
        let options = destroy::Command {
            non_interactive: true,
            quiet: true,
            ..options.clone()
        };
        let status = wsl
            .cli_exe()
            .arg("instance")
            .arg("destroy")
            .args(&options)
            .status()?;
        match status.code() {
            Some(exit_codes::INSTANCE_NOT_FOUND) => {}
            Some(0) => found = true,
            Some(c) => return Err(ExitCode::new(c).into()),
            None => anyhow::bail!("Interrupted"),
        }
    }

    let paths = Paths::get(name)?;
    for path in &paths.service_files {
        if path.exists() {
            found = true;
            log::info!(target: "edgedb::portable::destroy",
                       "Removing service file {:?}", path);
            fs::remove_file(path)?;
        }
    }
    Ok(found)
}

#[context("cannot read {:?}", path)]
fn read_wsl(path: &Path) -> anyhow::Result<WslInfo> {
    let reader = io::BufReader::new(fs::File::open(path)?);
    Ok(serde_json::from_reader(reader)?)
}

#[cfg(windows)]
#[context("cannot unpack debian distro from {:?}", zip_path)]
fn unpack_appx(zip_path: &Path, dest: &Path) -> anyhow::Result<()> {
    let mut zip = zip::ZipArchive::new(io::BufReader::new(fs::File::open(zip_path)?))?;
    let name = zip
        .file_names()
        .find(|name| {
            let lower = name.to_lowercase();
            lower.starts_with("distrolauncher-") && lower.ends_with("_x64.appx")
        })
        .ok_or_else(|| anyhow::anyhow!("file `DistroLauncher-*_x64.appx` is not found in archive"))?
        .to_string();
    let mut inp = zip.by_name(&name)?;
    let mut out = fs::File::create(dest)?;
    io::copy(&mut inp, &mut out)?;
    Ok(())
}

#[cfg(windows)]
#[context("cannot unpack root filesystem from {:?}", zip_path)]
fn unpack_root(zip_path: &Path, dest: &Path) -> anyhow::Result<()> {
    let mut zip = zip::ZipArchive::new(io::BufReader::new(fs::File::open(zip_path)?))?;
    let name = zip
        .file_names()
        .find(|name| name.eq_ignore_ascii_case("install.tar.gz"))
        .ok_or_else(|| anyhow::anyhow!("file `install.tar.gz` is not found in archive"))?
        .to_string();
    let mut inp = libflate::gzip::Decoder::new(io::BufReader::new(zip.by_name(&name)?))?;
    let mut out = fs::File::create(dest)?;
    io::copy(&mut inp, &mut out)?;
    Ok(())
}

#[cfg(windows)]
fn wsl_check_cli(_wsl: &wslapi::Library, wsl_info: &WslInfo) -> anyhow::Result<bool> {
    let self_ver = self_version()?;
    Ok(wsl_info
        .last_checked_version
        .as_ref()
        .map(|v| v != &self_ver)
        .unwrap_or(true))
}

#[cfg(windows)]
#[context("cannot check linux CLI version")]
fn wsl_cli_version(distro: &str) -> anyhow::Result<ver::Semver> {
    // Note: cannot capture output using wsl.launch

    use const_format::concatcp;
    let data = process::Native::new("check version", BRANDING_CLI_CMD, "wsl")
        .arg("--user")
        .arg("edgedb")
        .arg("--distribution")
        .arg(distro)
        .arg("_EDGEDB_FROM_WINDOWS=1")
        .arg(USR_BIN_EXE)
        .arg("--version")
        .get_stdout_text()?;
    let version = data
        .trim()
        .strip_prefix(concatcp!(BRANDING_CLI, " "))
        .with_context(|| format!("bad version info returned by linux CLI: {:?}", data))?
        .parse()?;
    Ok(version)
}

#[cfg(windows)]
fn download_binary(dest: &Path) -> anyhow::Result<()> {
    let my_ver = self_version()?;
    let (arch, _) = crate::portable::platform::get_cli()?
        .split_once('-')
        .unwrap();
    let platform = format!("{arch}-unknown-linux-musl");

    let pkgs = repository::get_platform_cli_packages(
        upgrade::channel(),
        &platform,
        repository::DEFAULT_TIMEOUT,
    )?;
    let pkg = pkgs.iter().find(|pkg| pkg.version == my_ver);
    let pkg = if let Some(pkg) = pkg {
        pkg.clone()
    } else {
        let pkg = repository::get_platform_cli_packages(
            upgrade::channel(),
            &platform,
            repository::DEFAULT_TIMEOUT,
        )?
        .into_iter()
        .max_by(|a, b| a.version.cmp(&b.version))
        .context("cannot find new version")?;
        if pkg.version < my_ver {
            return Err(bug::error(format!(
                "latest version of linux CLI {} \
                 is older that current windows CLI {}",
                pkg.version, my_ver
            )));
        }
        log::warn!(
            "No package matching version {} found. \
                    Using latest version {}.",
            my_ver,
            pkg.version
        );
        pkg
    };

    let down_path = dest.with_extension("download");
    let tmp_path = tmp_file_path(dest);
    download(&down_path, &pkg.url, false)?;
    upgrade::unpack_file(&down_path, &tmp_path, pkg.compression)?;
    fs_err::rename(&tmp_path, dest)?;

    Ok(())
}

#[cfg(windows)]
fn wsl_simple_cmd(wsl: &wslapi::Library, distro: &str, cmd: &str) -> anyhow::Result<()> {
    let code = wsl.launch_interactive(distro, cmd, /* current_working_dir */ false)?;
    if code != 0 {
        anyhow::bail!("WSL command {:?} exited with exit code: {}", cmd, code);
    }
    Ok(())
}

fn utf16_contains(bytes: &[u8], needle: &str) -> bool {
    use std::char::{REPLACEMENT_CHARACTER, decode_utf16};
    decode_utf16(
        bytes
            .chunks_exact(2)
            .map(|a| u16::from_le_bytes([a[0], a[1]])),
    )
    .map(|r| r.unwrap_or(REPLACEMENT_CHARACTER))
    .collect::<String>()
    .contains(needle)
}

#[cfg(windows)]
fn get_wsl_lib() -> anyhow::Result<&'static wslapi::Library> {
    static LIB: LazyLock<std::io::Result<wslapi::Library>> = LazyLock::new(wslapi::Library::new);
    match &*LIB {
        Ok(lib) => Ok(lib),
        Err(e) => anyhow::bail!("cannot initialize WSL (Windows Subsystem for Linux): {e:#}"),
    }
}

#[cfg(windows)]
#[context("cannot initialize WSL2 (windows subsystem for linux)")]
fn get_wsl_distro(install: bool) -> anyhow::Result<WslInit> {
    let wsl = get_wsl_lib()?;
    let meta_path = config_dir()?.join("wsl.json");
    let mut distro = None;
    let mut update_cli = true;
    let mut certs_timestamp = None;
    if meta_path.exists() {
        match read_wsl(&meta_path) {
            Ok(wsl_info) if wsl.is_distribution_registered(&wsl_info.distribution) => {
                update_cli = wsl_check_cli(wsl, &wsl_info)?;
                let update_certs =
                    wsl_info.certs_timestamp + CERT_UPDATE_INTERVAL < SystemTime::now();
                if !update_cli && !update_certs {
                    return Ok(WslInit {
                        distribution: wsl_info.distribution,
                    });
                }
                if !update_certs {
                    certs_timestamp = Some(wsl_info.certs_timestamp);
                }
                distro = Some(wsl_info.distribution);
            }
            Ok(_) => {}
            Err(e) => {
                log::warn!("Error reading WSL metadata: {e:#}");
            }
        }
    }
    let mut distro = distro.unwrap_or(CURRENT_DISTRO.to_string());

    let download_dir = cache_dir()?.join("downloads");
    fs::create_dir_all(&download_dir)?;

    if !wsl.is_distribution_registered(&distro) {
        update_cli = true;
        certs_timestamp = None;
        if !install {
            return Err(NoDistribution.into());
        }

        if let Some(use_distro) = Env::_wsl_distro()? {
            distro = use_distro;
        } else {
            let download_dir = cache_dir()?.join("downloads");
            fs::create_dir_all(&download_dir)?;

            let download_path = download_dir.join("debian.zip");
            download(&download_path, &DISTRO_URL, false)?;

            msg!("Unpacking WSL distribution...");
            let file_format = detect_file_format(&download_path)?;

            let root_path = download_dir.join("install.tar");
            match file_format {
                FileFormat::TarGz => {
                    // For .tar.gz files, we can directly extract the root filesystem
                    // without needing to unpack an appx first
                    msg!("Extracting root filesystem (tar.gz)...");
                    let mut file = fs::File::open(&download_path)?;
                    let mut out = fs::File::create(&root_path)?;
                    let mut decoder = libflate::gzip::Decoder::new(io::BufReader::new(file))?;
                    io::copy(&mut decoder, &mut out)?;
                }
                FileFormat::Zip => {
                    // For .zip files, we need to unpack the appx first, then extract the root
                    msg!("Extracting root filesystem (appx)...");
                    let appx_path = download_dir.join("debian.appx");
                    unpack_appx(&download_path, &appx_path)?;
                    unpack_root(&appx_path, &root_path)?;
                    fs::remove_file(&appx_path)?;
                }
            }

            let distro_path = wsl_dir()?.join(CURRENT_DISTRO);
            fs::create_dir_all(&distro_path)?;
            msg!("Initializing WSL distribution...");

            let result = process::Native::new("wsl check", "wsl", "wsl")
                .arg("--help")
                .get_output();

            match result {
                Ok(out) if !utf16_contains(&out.stdout, "--import") => {
                    return Err(anyhow::anyhow!(
                        "Current installed WSL version is outdated."
                    ))
                    .hint(
                        "Please run `wsl --install` under \
                               administrator privileges to upgrade.",
                    )?;
                }
                Ok(_) => {}
                Err(e) => {
                    return Err(anyhow::anyhow!("Error running `wsl` tool: {:#}", e)).hint(
                        "Requires Windows 10 version 2004 or higher \
                               (Build 19041 and above) or \
                               Windows 11.",
                    )?;
                }
            }

            let import_output = process::Native::new("wsl import", "wsl", "wsl")
                .arg("--import")
                .arg(CURRENT_DISTRO)
                .arg(&distro_path)
                .arg(&root_path)
                .arg("--version=2")
                .get_output()?;
            if !import_output.status.success() {
                // "Invalid command line argument: --version=2"
                if utf16_contains(&import_output.stdout, "--version=2") {
                    process::Native::new("wsl import", "wsl", "wsl")
                        .arg("--import")
                        .arg(CURRENT_DISTRO)
                        .arg(&distro_path)
                        .arg(&root_path)
                        .run()?;
                } else {
                    return Err(anyhow::anyhow!(
                        "Error importing WSL distribution: {:?} {:?}",
                        import_output.stderr,
                        import_output.stdout
                    ));
                }
            }

            fs::remove_file(&download_path)?;
            fs::remove_file(&root_path)?;

            distro = CURRENT_DISTRO.into();
        };

        wsl_simple_cmd(wsl, &distro, "useradd edgedb --uid 1000 --create-home")?;
    }

    if Env::_wsl_skip_update()? == Some(true) {
        update_cli = false;
        certs_timestamp = None;
    }

    if update_cli {
        msg!("Updating container CLI version...");
        if let Some(bin_path) = Env::_wsl_linux_binary()? {
            let bin_path = dunce::canonicalize(bin_path)?;
            wsl_simple_cmd(
                wsl,
                &distro,
                &format!(
                    "cp {} {USR_BIN_EXE} && chmod 755 {USR_BIN_EXE}",
                    shell_escape::unix::escape(path_to_linux(&bin_path)?.into()),
                ),
            )?;
        } else {
            let cache_path = download_dir.join("edgedb");
            download_binary(&cache_path)?;
            wsl_simple_cmd(
                wsl,
                &distro,
                &format!(
                    "mv {} {USR_BIN_EXE} && chmod 755 {USR_BIN_EXE}",
                    shell_escape::unix::escape(path_to_linux(&cache_path)?.into()),
                ),
            )?;
        };
    }

    let certs_timestamp = if let Some(ts) = certs_timestamp {
        ts
    } else {
        msg!("Checking certificate updates...");
        update_ca_certificates_manually(wsl, &distro)?;
        SystemTime::now()
    };

    let cli_version = wsl_cli_version(&distro)?;
    let my_ver = self_version()?;
    if cli_version < my_ver {
        return Err(bug::error(format!(
            "could not download correct version of CLI tools; \
            downloaded {}, expected {}",
            cli_version, my_ver
        )));
    }
    let info = WslInfo {
        distribution: distro.into(),
        last_checked_version: Some(my_ver),
        cli_timestamp: None,
        cli_version,
        certs_timestamp,
    };
    write_json(&meta_path, "WSL info", &info)?;
    return Ok(WslInit {
        distribution: info.distribution,
    });
}

#[cfg(unix)]
fn get_wsl_distro(_install: bool) -> anyhow::Result<WslInit> {
    Err(bug::error("WSL on unix is unupported"))
}

static WSL: Mutex<Option<WslInit>> = Mutex::new(None);

/// Ensures that WSL is initialized, installing it if necessary.
pub fn ensure_wsl() -> anyhow::Result<Wsl> {
    let mut wsl = WSL.lock().unwrap();
    if wsl.is_none() {
        *wsl = Some(get_wsl_distro(true)?);
    }
    Ok(Wsl {
        distribution: wsl.as_ref().unwrap().distribution.clone(),
        #[cfg(windows)]
        lib: get_wsl_lib()?,
    })
}

/// Get WSL if it's installed and initialized.
fn get_wsl() -> anyhow::Result<Option<Wsl>> {
    let mut wsl = WSL.lock().unwrap();
    if wsl.is_none() {
        match get_wsl_distro(false) {
            Ok(v) => *wsl = Some(v),
            Err(e) if e.is::<NoDistribution>() => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    Ok(Some(Wsl {
        distribution: wsl.as_ref().unwrap().distribution.clone(),
        #[cfg(windows)]
        lib: get_wsl_lib()?,
    }))
}

/// Get WSL if it's installed and initialized.
pub fn try_get_wsl() -> anyhow::Result<Wsl> {
    let mut wsl = WSL.lock().unwrap();
    if wsl.is_none() {
        match get_wsl_distro(false) {
            Ok(v) => *wsl = Some(v),
            Err(e) if e.is::<NoDistribution>() => {
                return Err(e).hint(formatcp!(
                    "WSL is initialized automatically on \
                  `{BRANDING_CLI_CMD} project init` or `{BRANDING_CLI_CMD} instance create`",
                ))?;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(Wsl {
        distribution: wsl.as_ref().unwrap().distribution.clone(),
        #[cfg(windows)]
        lib: get_wsl_lib()?,
    })
}

pub fn startup_dir() -> anyhow::Result<PathBuf> {
    Ok(dirs::data_dir()
        .context("cannot determine data directory")?
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup"))
}

fn service_file(instance: &str) -> anyhow::Result<PathBuf> {
    Ok(startup_dir()?.join(format!("edgedb-server-{instance}.cmd")))
}

pub fn service_files(name: &str) -> anyhow::Result<Vec<PathBuf>> {
    Ok(vec![service_file(name)?])
}

pub fn create_service(info: &InstanceInfo) -> anyhow::Result<()> {
    let wsl = try_get_wsl()?;
    create_and_start(&wsl, &info.name)
}

fn create_and_start(wsl: &Wsl, name: &str) -> anyhow::Result<()> {
    wsl.cli_exe()
        .arg("instance")
        .arg("start")
        .arg("-I")
        .arg(name)
        .run()?;
    // TODO: This should probably use _EDGEDB_FROM_WINDOWS=1 and --foreground
    fs_err::write(
        service_file(name)?,
        format!(
            "wsl \
            --distribution {} --user edgedb \
            _EDGEDB_FROM_WINDOWS=1 \
            {USR_BIN_EXE} instance start -I {name}",
            &wsl.distribution,
        ),
    )?;
    Ok(())
}

pub fn stop_and_disable(_name: &str) -> anyhow::Result<bool> {
    anyhow::bail!("running as a service is not yet supported on Windows");
}

pub fn server_cmd(instance: &str, _is_shutdown_supported: bool) -> anyhow::Result<process::Native> {
    let wsl = try_get_wsl()?;
    let mut pro = wsl.cli_exe();
    pro.arg("instance")
        .arg("start")
        .arg("--foreground")
        .arg("-I")
        .arg(instance);
    let instance = String::from(instance);
    pro.set_stop_process_command(move || {
        let mut cmd = tokio::process::Command::new("wsl");
        cmd.arg("--user").arg("edgedb");
        cmd.arg("--distribution").arg(&wsl.distribution);
        cmd.arg("_EDGEDB_FROM_WINDOWS=1");
        cmd.arg(USR_BIN_EXE);
        cmd.arg("instance").arg("stop").arg("-I").arg(&instance);
        cmd
    });
    pro.no_proxy();
    Ok(pro)
}

pub fn daemon_start(instance: &str) -> anyhow::Result<()> {
    let wsl = try_get_wsl()?;
    wsl.cli_exe()
        .arg("instance")
        .arg("start")
        .arg("-I")
        .arg(instance)
        .no_proxy()
        .run()?;
    Ok(())
}

pub fn start_service(_instance: &str) -> anyhow::Result<()> {
    anyhow::bail!("running as a service is not yet supported on Windows");
}

pub fn stop_service(_name: &str) -> anyhow::Result<()> {
    anyhow::bail!("running as a service is not yet supported on Windows");
}

pub fn restart_service(_inst: &InstanceInfo) -> anyhow::Result<()> {
    anyhow::bail!("running as a service is not yet supported on Windows");
}

pub fn service_status(_inst: &str) -> status::Service {
    status::Service::Inactive {
        error: "running as a service is not yet supported on Windows".into(),
    }
}

pub fn external_status(_inst: &InstanceInfo) -> anyhow::Result<()> {
    anyhow::bail!("running as a service is not yet supported on Windows");
}

pub fn is_wrapped() -> bool {
    let Ok(v) = Env::_from_windows() else {
        return false;
    };
    v.is_some()
}

pub fn install(options: &server::install::Command) -> anyhow::Result<()> {
    ensure_wsl()?
        .cli_exe()
        .arg("server")
        .arg("install")
        .args(options)
        .run()?;
    Ok(())
}

pub fn uninstall(options: &server::uninstall::Command) -> anyhow::Result<()> {
    if let Some(wsl) = get_wsl()? {
        wsl.cli_exe()
            .arg("server")
            .arg("uninstall")
            .args(options)
            .run()?;
    } else {
        log::warn!(
            "WSL distribution is not installed, \
                   so no {BRANDING} server versions are present."
        );
    }
    Ok(())
}

pub fn list_versions(options: &server::list_versions::Command) -> anyhow::Result<()> {
    if let Some(wsl) = get_wsl()? {
        wsl.cli_exe()
            .arg("server")
            .arg("list-versions")
            .args(options)
            .run()?;
    } else if options.json {
        println!("[]");
    } else {
        log::warn!(
            "WSL distribution is not installed, \
                   so no {BRANDING} server versions are present."
        );
    }
    Ok(())
}

pub fn info(options: &server::info::Command) -> anyhow::Result<()> {
    if let Some(wsl) = get_wsl()? {
        wsl.cli_exe()
            .arg("server")
            .arg("info")
            .args(options)
            .run()?;
    } else {
        anyhow::bail!(
            "WSL distribution is not installed, \
                       so no {BRANDING} server versions are present."
        );
    }
    Ok(())
}

pub fn reset_password(
    options: &instance::reset_password::Command,
    name: &str,
) -> anyhow::Result<()> {
    if let Some(wsl) = get_wsl()? {
        wsl.cli_exe()
            .arg("instance")
            .arg("reset-password")
            .args(options)
            .run()?;
        sync_credentials(name)?;
    } else {
        anyhow::bail!(
            "WSL distribution is not installed, \
                       so no {BRANDING} instances are present."
        );
    }
    Ok(())
}

pub fn start(options: &control::Start, name: &str) -> anyhow::Result<()> {
    if let Some(wsl) = get_wsl()? {
        if options.foreground {
            wsl.cli_exe()
                .arg("instance")
                .arg("start")
                .args(options)
                .run()?;
        } else {
            create_and_start(&wsl, name)?;
        }
    } else {
        anyhow::bail!(
            "WSL distribution is not installed, \
                       so no {BRANDING} instances are present."
        );
    }
    Ok(())
}

pub fn stop(options: &control::Stop, name: &str) -> anyhow::Result<()> {
    if let Some(wsl) = get_wsl()? {
        let service_file = service_file(name)?;
        fs::remove_file(&service_file)
            .map_err(|e| log::warn!("error removing {service_file:?}: {e:#}"))
            .ok();
        wsl.cli_exe()
            .arg("instance")
            .arg("stop")
            .args(options)
            .run()?;
    } else {
        anyhow::bail!(
            "WSL distribution is not installed, \
                       so no {BRANDING} instances are present."
        );
    }
    Ok(())
}

pub fn restart(options: &control::Restart) -> anyhow::Result<()> {
    if let Some(wsl) = get_wsl()? {
        wsl.cli_exe()
            .arg("instance")
            .arg("restart")
            .args(options)
            .run()?;
    } else {
        anyhow::bail!(
            "WSL distribution is not installed, \
                       so no {BRANDING} instances are present."
        );
    }
    Ok(())
}

pub fn logs(options: &control::Logs) -> anyhow::Result<()> {
    if let Some(wsl) = get_wsl()? {
        wsl.cli_exe()
            .arg("instance")
            .arg("logs")
            .args(options)
            .run()?;
    } else {
        anyhow::bail!(
            "WSL distribution is not installed, \
                       so no {BRANDING} instances are present."
        );
    }
    Ok(())
}

pub fn status(options: &status::Status) -> anyhow::Result<()> {
    if options.service {
        if let Some(wsl) = get_wsl()? {
            wsl.cli_exe()
                .arg("instance")
                .arg("status")
                .args(options)
                .run()?;
        } else {
            msg!(
                "WSL distribution is not installed, \
                   so no {BRANDING} instances are present."
            );
            return Err(ExitCode::new(exit_codes::INSTANCE_NOT_FOUND).into());
        }
    } else {
        let inner_opts = status::Status {
            quiet: true,
            ..options.clone()
        };
        if let Some(wsl) = get_wsl()? {
            let status = wsl
                .cli_exe()
                .arg("instance")
                .arg("status")
                .args(&inner_opts)
                .status()?;
            match status.code() {
                Some(exit_codes::INSTANCE_NOT_FOUND) => {}
                Some(0) => return Ok(()),
                Some(c) => return Err(ExitCode::new(c).into()),
                None => anyhow::bail!("Interrupted"),
            }
        } // else can only be remote instance
        status::remote_status(options)?;
    }
    Ok(())
}

fn list_local(options: &status::List) -> anyhow::Result<Vec<status::JsonStatus>> {
    if options.debug || options.extended {
        let inner_opts = status::List {
            quiet: true,
            no_remote: true,
            ..options.clone()
        };
        if let Some(wsl) = get_wsl()? {
            wsl.cli_exe()
                .arg("instance")
                .arg("list")
                .args(&inner_opts)
                .run()?;
        }
    }
    let inner_opts = status::List {
        no_remote: true,
        extended: false,
        debug: false,
        json: true,
        ..options.clone()
    };
    let local: Vec<status::JsonStatus> = if let Some(wsl) = get_wsl()? {
        let text = wsl
            .cli_exe()
            .arg("instance")
            .arg("list")
            .args(&inner_opts)
            .get_stdout_text()?;
        log::info!("WSL list returned {:?}", text);
        let mut instances: Vec<status::JsonStatus> = serde_json::from_str(&text)
            .context("cannot decode json from `instance list` in WSL")?;
        // Use the Windows service status, not the WSL one
        for instance in instances.iter_mut() {
            instance.service_status = status_str(&service_status(&instance.name))
                .to_owned()
                .into();
        }
        instances
    } else {
        Vec::new()
    };
    Ok(local)
}

pub fn list(options: &status::List, opts: &crate::Options) -> anyhow::Result<()> {
    let errors = Collector::new();
    let local = match list_local(options) {
        Ok(local) => local,
        Err(e) => {
            errors.add(e);
            Vec::new()
        }
    };
    let visited = local
        .iter()
        .map(|v| InstanceName::Local(v.name.clone()))
        .collect::<BTreeSet<_>>();

    let remote = if options.no_remote {
        Vec::new()
    } else {
        match status::get_remote(&visited, opts, &errors) {
            Ok(remote) => remote,
            Err(e) => {
                errors.add(e);
                Vec::new()
            }
        }
    };

    if local.is_empty() && remote.is_empty() {
        if status::print_errors(&errors.list(), false) {
            return Err(ExitCode::new(1).into());
        } else {
            if options.json {
                println!("[]");
            } else if !options.quiet {
                print::warn!("No instances found");
            }
            return Ok(());
        }
    }
    if options.debug {
        for status in remote {
            println!("{status:#?}");
        }
    } else if options.extended {
        for status in remote {
            status.print_extended();
        }
    } else if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &local
                    .into_iter()
                    .chain(remote.iter().map(|status| status.json()))
                    .collect::<Vec<_>>()
            )?
        );
    } else {
        status::print_table(&local, &remote);
    }

    if status::print_errors(&errors.list(), true) {
        Err(ExitCode::new(exit_codes::PARTIAL_SUCCESS).into())
    } else {
        Ok(())
    }
}

pub fn upgrade(options: &instance::upgrade::Command, name: &str) -> anyhow::Result<()> {
    let wsl = try_get_wsl()?;
    wsl.cli_exe()
        .arg("instance")
        .arg("upgrade")
        .args(options)
        .run()?;
    sync_credentials(name)?;
    Ok(())
}

pub fn revert(options: &instance::revert::Command, name: &str) -> anyhow::Result<()> {
    let wsl = try_get_wsl()?;
    wsl.cli_exe()
        .arg("instance")
        .arg("revert")
        .args(options)
        .run()?;
    sync_credentials(name)?;
    Ok(())
}

fn get_instance_data_dir(name: &str, wsl: &Wsl) -> anyhow::Result<PathBuf> {
    let data_dir = if name == "_localdev" {
        Env::server_dev_dir()?
            .unwrap_or_else(|| "/home/edgedb/.local/share/edgedb/_localdev/".into())
    } else {
        format!("/home/edgedb/.local/share/edgedb/data/{name}/").into()
    };

    if !wsl.check_path_exist(&data_dir) {
        anyhow::bail!(NonLocalInstance);
    }

    Ok(data_dir)
}

pub fn read_jws_key(name: &str) -> anyhow::Result<String> {
    let wsl = try_get_wsl()?;
    let data_dir = get_instance_data_dir(name, &wsl)?;
    for keys in ["edbjwskeys.pem", "edbjwskeys.json"] {
        if wsl.check_path_exist(data_dir.join(keys)) {
            return wsl.read_text_file(data_dir.join(keys));
        }
    }
    anyhow::bail!("No JWS keys found for instance {name}");
}

pub fn get_instance_info(name: &str) -> anyhow::Result<String> {
    let wsl = try_get_wsl()?;
    wsl.read_text_file(format!(
        "/home/edgedb/.local/share/edgedb/data/{name}/instance_info.json"
    ))
}

pub fn is_in_wsl() -> bool {
    *IS_IN_WSL != WslState::NotWsl
}

pub fn is_in_wsl1() -> bool {
    *IS_IN_WSL == WslState::Wsl1
}

pub fn extension_install(cmd: &extension::ExtensionInstall) -> anyhow::Result<()> {
    let wsl = try_get_wsl()?;

    wsl.cli_exe()
        .arg("instance")
        .arg("install")
        .args(cmd)
        .run()?;
    Ok(())
}

pub fn extension_uninstall(cmd: &extension::ExtensionUninstall) -> anyhow::Result<()> {
    let wsl = try_get_wsl()?;

    wsl.cli_exe()
        .arg("instance")
        .arg("uninstall")
        .args(cmd)
        .run()?;
    Ok(())
}

#[cfg(windows)]
fn update_ca_certificates_manually(wsl: &wslapi::Library, distro: &str) -> anyhow::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let temp_file = temp_dir.path().join("windows_cert_update.sh");
    let script = include_str!("windows_cert_update.sh").replace("\r\n", "\n");
    std::fs::write(&temp_file, script)?;

    copy_in(wsl, distro, temp_file, "/tmp/windows_cert_update.sh")?;

    let mut process = process::Native::new("update certificates", "certs", "wsl");
    process
        .arg("--distribution")
        .arg(distro)
        .arg("bash")
        .arg("-c")
        .arg("chmod a+x /tmp/windows_cert_update.sh && /tmp/windows_cert_update.sh");

    if *Env::in_ci()?.unwrap_or_default() {
        process.run()?;
    } else {
        let output = process.get_stdout_text()?;
    }

    Ok(())
}

fn detect_file_format(file_path: &Path) -> anyhow::Result<FileFormat> {
    let mut file = fs::File::open(file_path)?;
    let mut buffer = [0u8; 2];
    file.read_exact(&mut buffer)?;

    // Check for gzip magic number (0x1f 0x8b)
    if buffer == [0x1f, 0x8b] {
        Ok(FileFormat::TarGz)
    } else {
        // Assume it's a zip file (PK header)
        Ok(FileFormat::Zip)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum FileFormat {
    TarGz,
    Zip,
}
