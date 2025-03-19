use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use fn_error_context::context;
use rand::{Rng, thread_rng};
use serde::{Deserialize, Serialize};

use crate::branding::BRANDING_CLI_CMD;
use crate::cli;
use crate::cli::env::Env;
use crate::platform;
use crate::portable::repository;
use crate::portable::ver;

#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    #[serde(with = "humantime_serde")]
    timestamp: SystemTime,
    #[serde(with = "humantime_serde")]
    expires: SystemTime,
    version: Option<ver::Semver>,
}

fn cache_age() -> Duration {
    Duration::from_secs(thread_rng().gen_range(16 * 3600..32 * 3600))
}

fn negative_cache_age() -> Duration {
    Duration::from_secs(thread_rng().gen_range(6 * 3600..12 * 3600))
}

impl Cache {
    fn channel_matches(&self, chan: &repository::Channel) -> bool {
        self.version
            .as_ref()
            .map(|v| cli::upgrade::channel_of(&v.to_string()) == *chan)
            .unwrap_or(true) // negative cache always matches
    }
}

fn read_cache(dir: &Path) -> anyhow::Result<Cache> {
    let file = fs::File::open(dir.join("version_check.json"))?;
    let reader = std::io::BufReader::new(file);
    Ok(serde_json::from_reader(reader)?)
}

#[context("error writing {}/version_check.json", dir.display())]
fn write_cache(dir: &Path, data: &Cache) -> anyhow::Result<()> {
    let file = fs::File::create(dir.join("version_check.json"))?;
    let writer = std::io::BufWriter::new(file);
    Ok(serde_json::to_writer_pretty(writer, data)?)
}

fn newer_warning(ver: &ver::Semver) {
    if cli::upgrade::can_upgrade() {
        eprintln!(
            "Newer version of {BRANDING_CLI_CMD} tool exists {} (current {}). \
                To upgrade run `{BRANDING_CLI_CMD} cli upgrade`",
            ver,
            env!("CARGO_PKG_VERSION")
        );
    } else {
        eprintln!(
            "Newer version of {BRANDING_CLI_CMD} tool exists {} (current {})",
            ver,
            env!("CARGO_PKG_VERSION")
        );
    }
}

fn _check(cache_dir: &Path, strict: bool) -> anyhow::Result<()> {
    let self_version = cli::upgrade::self_version()?;
    let channel = cli::upgrade::channel();
    match read_cache(cache_dir) {
        Ok(cache) if cache.expires > SystemTime::now() && cache.channel_matches(&channel) => {
            log::debug!("Cached version {:?}", cache.version);
            if let Some(ver) = cache.version {
                if self_version < ver {
                    newer_warning(&ver);
                }
            }
            return Ok(());
        }
        Ok(_) => {}
        Err(e) => {
            if strict {
                return Err(e).context("error reading CLI version cache");
            }
            log::debug!("Error reading cache: {}", e);
        }
    }
    let timestamp = SystemTime::now();
    let pkg = repository::get_cli_packages(channel, Duration::new(1, 0))
        .map_err(|e| log::info!("cli version check failed: {e:#}"))
        .ok()
        .and_then(|pkgs| pkgs.into_iter().map(|pkg| pkg.version).max());
    if let Some(ver) = &pkg {
        if &self_version < ver {
            newer_warning(ver);
        }
    }
    log::debug!("Remote version {:?}", pkg);
    write_cache(
        cache_dir,
        &Cache {
            timestamp,
            expires: timestamp
                + if pkg.is_some() {
                    cache_age()
                } else {
                    negative_cache_age()
                },
            version: pkg.clone(),
        },
    )?;
    Ok(())
}

fn cache_dir() -> anyhow::Result<PathBuf> {
    let dir = platform::cache_dir()?;
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn check(no_version_check_opt: bool) -> anyhow::Result<()> {
    use cli::env::VersionCheck;
    let mut strict = false;
    if no_version_check_opt {
        log::debug!("Skipping version check due to --no-cli-update-check");
        return Ok(());
    }
    match Env::run_version_check()?.unwrap_or(VersionCheck::Default) {
        VersionCheck::Never => {
            log::debug!(
                "EDGEDB_RUN_VERSION_CHECK set to `never`, \
                skipping version check
                "
            );
            return Ok(());
        }
        VersionCheck::Cached | VersionCheck::Default => {}
        VersionCheck::Strict => {
            strict = true;
        }
    }
    let dir = match cache_dir() {
        Ok(dir) => dir,
        Err(e) => {
            if strict {
                return Err(e).context("Version check failed");
            }
            log::debug!("Version check ignored: {}", e);
            return Ok(());
        }
    };
    match _check(&dir, strict) {
        Ok(()) => {}
        Err(e) => {
            if strict {
                return Err(e).context("Cannot check for updates");
            }
            log::warn!("Cannot check for updates: {:#}", e);
        }
    }
    Ok(())
}
