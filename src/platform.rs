use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Context;
use fn_error_context::context;

use crate::branding::{
    BRANDING_CLI_CMD_FILE, BRANDING_SCHEMA_FILE_EXT_ESDL, BRANDING_SCHEMA_FILE_EXT_GEL,
};
use crate::cli::env::Env;

#[cfg(windows)]
pub type Uid = u32;

#[cfg(not(windows))]
pub type Uid = libc::uid_t;

#[cfg(windows)]
pub fn get_current_uid() -> Uid {
    unreachable!();
}

#[cfg(not(windows))]
pub fn get_current_uid() -> Uid {
    unsafe { libc::geteuid() }
}

pub fn cache_dir() -> anyhow::Result<PathBuf> {
    let dir = if cfg!(windows) {
        dirs::data_local_dir()
            .context("cannot determine local data directory")?
            .join("EdgeDB")
            .join("cache")
    } else {
        dirs::cache_dir()
            .context("cannot determine cache directory")?
            .join("edgedb")
    };
    Ok(dir)
}

pub fn home_dir() -> anyhow::Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))
}

pub fn config_dir() -> anyhow::Result<PathBuf> {
    let dir = if cfg!(windows) {
        dirs::data_local_dir()
            .context("cannot determine local data directory")?
            .join("EdgeDB")
            .join("config")
    } else {
        dirs::config_dir()
            .context("cannot determine config directory")?
            .join("edgedb")
    };
    Ok(dir)
}

pub fn tmp_file_name(path: &Path) -> OsString {
    if let Some(file_name) = path.file_name() {
        let mut buf = OsString::with_capacity(6 + file_name.len());
        buf.push(".~");
        buf.push(file_name);
        buf.push(".tmp");
        buf
    } else {
        OsString::from(".~.tmp") // should never be relied on in practice
    }
}

pub fn tmp_file_path(path: &Path) -> PathBuf {
    path.parent()
        .unwrap_or(Path::new("."))
        .join(tmp_file_name(path))
}

#[cfg(unix)]
pub fn path_bytes(path: &Path) -> anyhow::Result<&[u8]> {
    use std::os::unix::ffi::OsStrExt;
    Ok(path.as_os_str().as_bytes())
}

#[cfg(windows)]
pub fn path_bytes(path: &Path) -> anyhow::Result<&[u8]> {
    let s = path
        .to_str()
        // should never happen because paths on windows are valid UTF-16
        .ok_or_else(|| anyhow::anyhow!("bad chars in path"))?;
    Ok(s.as_bytes())
}

#[cfg(unix)]
pub fn bytes_to_path(path: &[u8]) -> anyhow::Result<&Path> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    Ok(Path::new(OsStr::from_bytes(path)))
}

#[cfg(windows)]
pub fn bytes_to_path(path: &[u8]) -> anyhow::Result<&'_ Path> {
    use anyhow::Context;

    let s = std::str::from_utf8(path).context("bad chars in path")?;
    Ok(Path::new(s))
}

#[cfg(unix)]
pub fn symlink_dir(original: impl AsRef<Path>, path: impl AsRef<Path>) -> anyhow::Result<()> {
    std::os::unix::fs::symlink(original, path)?;
    Ok(())
}

#[cfg(windows)]
pub fn symlink_dir(original: impl AsRef<Path>, path: impl AsRef<Path>) -> anyhow::Result<()> {
    std::os::windows::fs::symlink_dir(original, path)?;
    Ok(())
}

pub fn binary_path() -> anyhow::Result<PathBuf> {
    let dir = match dirs::executable_dir() {
        Some(dir) => dir,
        // windows and macos fit this branch
        None => dirs::data_dir()
            .context("cannot determine local data directory")?
            .join("edgedb")
            .join("bin"),
    };
    Ok(dir.join(BRANDING_CLI_CMD_FILE))
}

pub fn is_legacy_schema_file(filename: &str) -> bool {
    filename.ends_with(&format!(".{BRANDING_SCHEMA_FILE_EXT_ESDL}"))
}

pub fn is_schema_file(filename: &str) -> bool {
    filename.ends_with(&format!(".{BRANDING_SCHEMA_FILE_EXT_ESDL}"))
        || filename.ends_with(&format!(".{BRANDING_SCHEMA_FILE_EXT_GEL}"))
}

pub fn data_dir() -> anyhow::Result<PathBuf> {
    Ok(dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("Can't determine data directory"))?
        .join("edgedb")
        .join("data"))
}

pub fn portable_dir() -> anyhow::Result<PathBuf> {
    Ok(dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("Can't determine data directory"))?
        .join("edgedb")
        .join("portable"))
}

#[cfg_attr(unix, allow(dead_code))]
pub fn wsl_dir() -> anyhow::Result<PathBuf> {
    Ok(dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("Can't determine data directory"))?
        .join("edgedb")
        .join("wsl"))
}

#[context("cannot determine running executable path")]
pub fn current_exe() -> anyhow::Result<PathBuf> {
    Ok(env::current_exe()?)
}

pub fn detect_ipv6() -> bool {
    std::net::TcpListener::bind(std::net::SocketAddrV6::new(
        std::net::Ipv6Addr::LOCALHOST,
        0, // dynamicallly alocated port
        0, // no flow info
        0, // no scope id
    ))
    .is_ok()
}

pub fn editor_path() -> anyhow::Result<String> {
    Ok(if let Some(editor) = Env::editor()? {
        editor
    } else if let Some(editor) = Env::system_editor()? {
        editor
    } else if cfg!(windows) {
        String::from("notepad.exe")
    } else {
        String::from("vi")
    })
}

pub fn pager_path() -> anyhow::Result<String> {
    Ok(if let Some(pager) = Env::pager()? {
        pager
    } else if let Some(pager) = Env::system_pager()? {
        pager
    } else if cfg!(windows) {
        String::from("more.com")
    } else {
        String::from("less -R")
    })
}

pub async fn spawn_editor(path: &Path) -> anyhow::Result<()> {
    let editor = editor_path()?;
    let mut items = editor.split_whitespace();
    let mut cmd = tokio::process::Command::new(items.next().unwrap());
    cmd.args(items);
    cmd.arg(path);
    let res = cmd.status().await?;
    if res.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("editor exited with: {}", res))
    }
}
