use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::Context;
use gel_cli_derive::IntoArgs;
use fn_error_context::context;
use indicatif::{ProgressBar, ProgressStyle};
use once_cell::sync::Lazy;

use crate::branding::BRANDING_CLI_CMD;
use crate::commands::ExitCode;
use crate::platform;
use crate::portable::exit_codes;
use crate::portable::local::{InstallInfo, write_json};
use crate::portable::platform::optional_docker_check;
use crate::portable::repository::Channel;
use crate::portable::repository::QueryOptions;
use crate::portable::repository::{PackageHash, PackageInfo, Query, download};
use crate::portable::repository::{get_server_package, get_specific_package};
use crate::portable::ver::{self, Build};
use crate::print::{self, Highlight, msg};

static INSTALLED_VERSIONS: Lazy<Mutex<BTreeSet<Build>>> = Lazy::new(|| Mutex::new(BTreeSet::new()));

pub fn run(options: &Command) -> anyhow::Result<()> {
    if optional_docker_check()? {
        print::error!("`{BRANDING_CLI_CMD} server install` not supported in Docker containers.");
        Err(ExitCode::new(exit_codes::DOCKER_CONTAINER))?;
    }
    let (query, _) = Query::from_options(
        QueryOptions {
            nightly: options.nightly,
            stable: false,
            testing: false,
            channel: options.channel,
            version: options.version.as_ref(),
        },
        || Ok(Query::stable()),
    )?;
    version(&query)?;
    Ok(())
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    #[arg(short = 'i', long)]
    pub interactive: bool,
    #[arg(long, conflicts_with_all=&["channel", "version"])]
    pub nightly: bool,
    #[arg(long, conflicts_with_all=&["nightly", "channel"])]
    pub version: Option<ver::Filter>,
    #[arg(long, conflicts_with_all=&["nightly", "version"])]
    #[arg(value_enum)]
    pub channel: Option<Channel>,
}

pub fn version(query: &Query) -> anyhow::Result<InstallInfo> {
    let pkg_info = get_server_package(query)?.context("no package matching your criteria found")?;
    ver::print_version_hint(&pkg_info.version.specific(), query);
    package(&pkg_info)
}

pub fn specific(version: &ver::Specific) -> anyhow::Result<InstallInfo> {
    let target_dir = platform::portable_dir()?.join(version.to_string());
    if target_dir.exists() {
        return InstallInfo::read(&target_dir);
    }
    let pkg =
        get_specific_package(version)?.with_context(|| format!("cannot find package {version}"))?;
    package(&pkg)
}

pub fn package(pkg_info: &PackageInfo) -> anyhow::Result<InstallInfo> {
    let ver_name = pkg_info.version.specific().to_string();
    let target_dir = platform::portable_dir()?.join(ver_name);
    if target_dir.exists() {
        let meta = check_metadata(&target_dir, pkg_info)?;
        if INSTALLED_VERSIONS
            .lock()
            .unwrap()
            .insert(meta.version.clone())
        {
            msg!(
                "Version {} is already downloaded",
                meta.version.to_string().emphasized()
            );
        }
        return Ok(meta);
    }

    msg!("Downloading package...");
    let cache_path = download_package(pkg_info)?;
    let tmp_target = platform::tmp_file_path(&target_dir);
    unpack_package(&cache_path, &tmp_target)?;
    let info = InstallInfo {
        version: pkg_info.version.clone(),
        package_url: pkg_info.url.clone(),
        package_hash: pkg_info.hash.clone(),
        installed_at: SystemTime::now(),
        slot: pkg_info.slot.clone(),
    };
    write_json(&tmp_target.join("install_info.json"), "metadata", &info)?;
    fs::rename(&tmp_target, &target_dir)
        .with_context(|| format!("cannot rename {tmp_target:?} -> {target_dir:?}"))?;
    unlink_cache(&cache_path);
    msg!(
        "Successfully installed {}",
        pkg_info.version.to_string().emphasized()
    );
    INSTALLED_VERSIONS
        .lock()
        .unwrap()
        .insert(pkg_info.version.clone());

    Ok(info)
}

#[context("metadata error for {:?}", dir)]
fn check_metadata(dir: &Path, pkg_info: &PackageInfo) -> anyhow::Result<InstallInfo> {
    let data = InstallInfo::read(dir)?;
    if data.version != pkg_info.version {
        log::warn!(
            "Remote package has version {},
                    installed package version: {}",
            pkg_info.version,
            data.version
        );
    }
    log::info!(
        "Package {} was installed at {}, location: {:?}",
        data.version,
        humantime::format_rfc3339(data.installed_at),
        dir
    );
    Ok(data)
}

#[context("failed to download {}", pkg_info)]
pub fn download_package(pkg_info: &PackageInfo) -> anyhow::Result<PathBuf> {
    let cache_dir = platform::cache_dir()?;
    let download_dir = cache_dir.join("downloads");
    fs::create_dir_all(&download_dir)?;
    let cache_path = download_dir.join(pkg_info.cache_file_name());
    let hash = download(&cache_path, &pkg_info.url, false)?;
    match &pkg_info.hash {
        PackageHash::Blake2b(hex) => {
            if hash.to_hex()[..] != hex[..] {
                anyhow::bail!("hash mismatch {} != {}", hash.to_hex(), hex);
            }
        }
        PackageHash::Unknown(val) => {
            log::warn!("Cannot verify hash, unknown hash format {:?}", val);
        }
    }
    Ok(cache_path)
}

fn build_path(base: &Path, path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let mut components = path.components().filter_map(|part| {
        match part {
            Component::Normal(part) => Some(Ok(part)),
            // Leading '/' characters, root paths, and '.'
            // components are just ignored and treated as "empty
            // components"
            Component::Prefix(..) | Component::RootDir | Component::CurDir => None,
            // If any part of the filename is '..', then skip over
            // unpacking the file to prevent directory traversal
            // security issues.  See, e.g.: CVE-2001-1267,
            // CVE-2002-0399, CVE-2005-1918, CVE-2007-4131
            Component::ParentDir => Some(Err(anyhow::anyhow!("erroneous path {:?}", path))),
        }
    });
    if let Some(directory_name) = components.next() {
        directory_name?;
    } else {
        return Ok(None); // skipping root
    }

    let mut dest = PathBuf::from(base);
    if let Some(component) = components.next() {
        dest.push(component?);
    } else {
        return Ok(None); // the package directory itself
    }
    for component in components {
        let component = component?;
        match dest.symlink_metadata() {
            Ok(m) if m.file_type().is_symlink() => {
                anyhow::bail!("cannot unpack {:?} to the symlinked dir {:?}", path, dest);
            }
            Ok(m) if m.file_type().is_file() => {
                anyhow::bail!("{:?} is a file, not a directory for {:?}", dest, path);
            }
            Ok(_) => {}
            Err(_) => {
                fs::create_dir(&dest)?;
            }
        }
        dest.push(component);
    }
    Ok(Some(dest))
}

#[context("failed to unpack {:?} -> {:?}", cache_file, target_dir)]
fn unpack_package(cache_file: &Path, target_dir: &Path) -> anyhow::Result<()> {
    if target_dir.exists() {
        fs::remove_dir_all(target_dir)?;
    }
    fs::create_dir_all(target_dir)?;

    // needed for long paths on windows
    let target_dir = target_dir.canonicalize()?;

    let file = fs::File::open(cache_file)?;
    let bar = ProgressBar::new(file.metadata()?.len());
    bar.set_style(
        ProgressStyle::default_bar()
            .template("Unpacking [{bar}] {bytes:>7.dim}/{total_bytes:7}")
            .expect("template is ok")
            .progress_chars("=> "),
    );
    let file = zstd::Decoder::new(io::BufReader::new(bar.wrap_read(file)))?;
    let mut arch = tar::Archive::new(file);

    for entry in arch.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if let Some(path) = build_path(&target_dir, &path)? {
            entry.unpack(path)?;
        }
    }
    bar.finish_and_clear();
    Ok(())
}

fn unlink_cache(cache_file: &Path) {
    fs::remove_file(cache_file)
        .map_err(|e| {
            log::warn!("Failed to remove cache {:?}: {}", cache_file, e);
        })
        .ok();
}
