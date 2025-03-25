use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::Context;
use fn_error_context::context;
use fs_err as fs;

use gel_dsn::gel::CredentialsFile;
use gel_tokio::{Config, InstanceName};

use crate::platform::{config_dir, tmp_file_name};
use crate::question;

pub fn base_dir() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("credentials"))
}

pub fn path(name: &str) -> anyhow::Result<PathBuf> {
    Ok(base_dir()?.join(format!("{name}.json")))
}

pub fn all_instance_names() -> anyhow::Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    let dir = base_dir()?;
    let dir_entries = match fs::read_dir(&dir) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(result),
        Err(e) => return Err(e).context(format!("error reading {dir:?}")),
    };
    for item in dir_entries {
        let item = item?;
        if let Ok(filename) = item.file_name().into_string() {
            if let Some(name) = filename.strip_suffix(".json") {
                if InstanceName::from_str(name).is_ok() {
                    result.insert(name.into());
                }
            }
        }
    }
    Ok(result)
}

#[tokio::main(flavor = "current_thread")]
#[context("cannot write credentials file {}", path.display())]
pub async fn write(path: &Path, credentials: &CredentialsFile) -> anyhow::Result<()> {
    write_async(path, credentials).await?;
    Ok(())
}

#[context("cannot write credentials file {}", path.display())]
pub async fn write_async(path: &Path, credentials: &CredentialsFile) -> anyhow::Result<()> {
    use tokio::fs;

    fs::create_dir_all(path.parent().unwrap()).await?;
    let tmp_path = path.with_file_name(tmp_file_name(path));
    fs::write(&tmp_path, serde_json::to_vec_pretty(&credentials)?).await?;
    fs::rename(&tmp_path, path).await?;
    Ok(())
}

pub async fn read(path: &Path) -> anyhow::Result<CredentialsFile> {
    use tokio::fs;

    let text = fs::read_to_string(path).await?;
    Ok(serde_json::from_str(&text)?)
}

pub fn read_sync(path: &Path) -> anyhow::Result<CredentialsFile> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

pub fn maybe_update_credentials_file(config: &Config, ask: bool) -> anyhow::Result<()> {
    if let Some(instance_name) = config.local_instance_name() {
        if let Ok(creds_path) = path(instance_name) {
            if let Ok(creds) = config.as_credentials() {
                let new = serde_json::to_value(&creds)?;
                let old = serde_json::from_str::<serde_json::Value>(
                    &std::fs::read_to_string(&creds_path).unwrap_or_default(),
                )?;
                if new != old {
                    log::debug!("old: {old}");
                    log::debug!("new: {new}");
                    if !ask
                        || question::Confirm::new(format!(
                            "The format of the instance credential file at {} is outdated, \
                    update now?",
                            creds_path.display(),
                        ))
                        .ask()?
                    {
                        std::fs::write(&creds_path, new.to_string())?;
                    }
                }
            }
        }
    }
    Ok(())
}
