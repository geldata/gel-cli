use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use fn_error_context::context;
use fs_err as fs;
use indicatif::{ProgressBar, ProgressStyle};

use crate::platform::{binary_path, current_exe, home_dir, tmp_file_path};
use crate::portable::platform;
use crate::portable::repository::{self, Channel, download};
use crate::portable::ver;
use crate::print::{self, Highlight, msg};
use crate::process;

const INDEX_TIMEOUT: Duration = Duration::new(60, 0);

#[derive(clap::Args, Clone, Debug)]
pub struct Command {
    /// Enable verbose output
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// Disable progress output
    #[arg(short = 'q', long)]
    pub quiet: bool,
    /// Force reinstall even if no newer version exists
    #[arg(long)]
    pub force: bool,
    /// Upgrade to latest nightly version
    #[arg(long)]
    #[arg(conflicts_with_all=&["to_testing", "to_stable", "to_channel"])]
    pub to_nightly: bool,
    /// Upgrade to latest stable version
    #[arg(long)]
    #[arg(conflicts_with_all=&["to_testing", "to_nightly", "to_channel"])]
    pub to_stable: bool,
    /// Upgrade to latest testing version
    #[arg(long)]
    #[arg(conflicts_with_all=&["to_stable", "to_nightly", "to_channel"])]
    pub to_testing: bool,
    /// Upgrade specified instance to specified channel
    #[arg(long, value_enum)]
    #[arg(conflicts_with_all=&["to_stable", "to_nightly", "to_testing"])]
    pub to_channel: Option<Channel>,
}

pub fn run(cmd: &Command) -> anyhow::Result<()> {
    upgrade(cmd, _get_upgrade_path()?)
}

fn upgrade(cmd: &Command, path: PathBuf) -> anyhow::Result<()> {
    let cur_channel = channel();
    let channel = if let Some(channel) = cmd.to_channel {
        channel
    } else if cmd.to_stable {
        Channel::Stable
    } else if cmd.to_nightly {
        Channel::Nightly
    } else if cmd.to_testing {
        Channel::Testing
    } else {
        cur_channel
    };

    #[allow(unused_mut)]
    let mut target_plat = platform::get_cli()?;
    // Always force upgrade when switching channel
    #[allow(unused_mut)]
    let mut force = cmd.force || cur_channel != channel;

    if cfg!(all(target_os = "macos", target_arch = "x86_64")) && platform::is_arm64_hardware() {
        target_plat = "aarch64-apple-darwin";
        // Always force upgrade when need to switch platform
        force = true;
    }

    let pkg = repository::get_platform_cli_packages(channel, target_plat, INDEX_TIMEOUT)?
        .into_iter()
        .max_by(|a, b| a.version.cmp(&b.version))
        .context("cannot find new version")?;
    if !force && pkg.version <= self_version()? {
        log::info!("Version is identical; no update needed.");
        if !cmd.quiet {
            print::success!("Already up to date.");
        }
        return Ok(());
    }

    let down_dir = path
        .parent()
        .context("download path missing directory component")?;
    fs::create_dir_all(down_dir).with_context(|| format!("failed to create {down_dir:?}"))?;

    let down_path = path.with_extension("download");
    let tmp_path = tmp_file_path(&path);

    download(&down_path, &pkg.url, cmd.quiet)?;
    unpack_file(&down_path, &tmp_path, pkg.compression)?;

    let backup_path = path.with_extension("backup");
    if cfg!(unix) {
        fs::remove_file(&backup_path).ok();
        fs::hard_link(&path, &backup_path)
            .map_err(|e| log::warn!("Cannot keep a backup file: {:#}", e))
            .ok();
    } else if cfg!(windows) {
        fs::remove_file(&backup_path).ok();
        fs::rename(&path, &backup_path)?;
    } else {
        anyhow::bail!("unknown OS");
    }
    process::Native::new("upgrade", "cli", &tmp_path)
        .arg("cli")
        .arg("install")
        .arg("--upgrade")
        .arg("--no-modify-path")
        .arg("--installation-path")
        .arg(down_dir)
        .no_proxy()
        .run()?;
    fs::remove_file(&tmp_path).ok();
    if !cmd.quiet {
        msg!(
            "Upgraded to version {}",
            pkg.version.to_string().emphasized()
        );
    }
    Ok(())
}

pub fn can_upgrade() -> bool {
    _get_upgrade_path().is_ok()
}

fn _get_upgrade_path() -> anyhow::Result<PathBuf> {
    let exe_path = current_exe()?;
    let home = home_dir()?;
    if !exe_path.starts_with(&home) {
        anyhow::bail!("Only binary installed under {:?} can be upgraded", home);
    }
    Ok(exe_path)
}

#[context("error unpacking {:?} -> {:?}", src, tgt)]
pub fn unpack_file(
    src: &Path,
    tgt: &Path,
    compression: Option<repository::Compression>,
) -> anyhow::Result<()> {
    fs::remove_file(tgt).ok();
    match compression {
        Some(repository::Compression::Zstd) => {
            fs::remove_file(tgt).ok();
            let src_f = fs::File::open(src)?;

            let mut opt = fs::OpenOptions::new();
            opt.write(true).create_new(true);
            #[cfg(unix)]
            {
                use fs_err::os::unix::fs::OpenOptionsExt;
                opt.mode(0o755);
            }
            let mut tgt_f = opt.open(tgt)?;

            let bar = ProgressBar::new(src.metadata()?.len());
            bar.set_style(
                ProgressStyle::default_bar()
                    .template("Unpacking [{bar}] {bytes:>7.dim}/{total_bytes:7}")
                    .expect("template is ok")
                    .progress_chars("=> "),
            );
            let mut decoded = zstd::Decoder::new(io::BufReader::new(bar.wrap_read(src_f)))?;
            io::copy(&mut decoded, &mut tgt_f)?;
            fs::remove_file(src).ok();
            Ok(())
        }
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(src, PermissionsExt::from_mode(0o755))?;
            }
            fs::rename(src, tgt)?;
            Ok(())
        }
    }
}

pub fn channel_of(ver: &str) -> repository::Channel {
    if ver.contains("-dev.") {
        Channel::Nightly
    } else if ver.contains('-') {
        Channel::Testing
    } else {
        Channel::Stable
    }
}

pub fn channel() -> repository::Channel {
    channel_of(env!("CARGO_PKG_VERSION"))
}

pub fn self_version() -> anyhow::Result<ver::Semver> {
    env!("CARGO_PKG_VERSION")
        .parse()
        .context("cannot parse cli version")
}

pub fn upgrade_to_arm64() -> anyhow::Result<()> {
    upgrade(
        &Command {
            verbose: false,
            quiet: false,
            force: true,
            to_nightly: false,
            to_stable: false,
            to_testing: false,
            to_channel: None,
        },
        binary_path()?,
    )
}
