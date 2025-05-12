use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

use gel_tokio::InstanceName;
use log::{debug, warn};
use serde_json::json;

use crate::portable::project::get_stash_path;

const LOCK_FILE_NAME: &str = "gel-cli.lock";
const SLOW_LOCK_WARNING: Duration = Duration::from_secs(10);
const SLOW_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct InstanceLock {
    inner: Arc<LockInner>,
}

impl InstanceLock {
    /// Notify the lock that it will be removed externally.
    pub fn lock_will_be_removed(&self) {
        if let LockInner::Local(_, _, must_exist) = &*self.inner {
            must_exist.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[derive(Debug)]
enum LockInner {
    NoLock,
    Local(
        PathBuf,
        Option<file_guard::FileGuard<Arc<File>>>,
        AtomicBool,
    ),
}

impl Drop for LockInner {
    fn drop(&mut self) {
        if let LockInner::Local(path, lock, must_exist) = self {
            if let Some(lock) = lock.take() {
                drop(lock);
                if let Err(e) = std::fs::remove_file(&path) {
                    if must_exist.load(std::sync::atomic::Ordering::Relaxed) {
                        warn!("Failed to remove lock file {path:?}: {e}");
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectLock {
    inner: Arc<LockInner>,
}

pub struct LockManager {}

#[derive(thiserror::Error, Debug)]
pub enum LockError {
    #[error("Could not acquire lock being held by process {pid} running {cmd:?}")]
    Locked { pid: u32, cmd: String },
    #[error("Lock file {path:?} is missing or corrupt")]
    BadLockFile { path: PathBuf },
    #[error("I/O error: {error}")]
    IOError {
        path: PathBuf,
        error: std::io::Error,
    },
}

fn try_create_lock(lock_type: file_guard::Lock, path: PathBuf) -> Result<LockInner, LockError> {
    let mut lock_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .unwrap();
    lock_file.write_all(
        json!({"pid": std::process::id(), "cmd": std::env::args().collect::<Vec<_>>().join(" ")})
            .to_string()
            .as_bytes(),
    ).map_err(|e| LockError::IOError { path: path.clone(), error: e })?;
    let lock_file = Arc::new(lock_file);
    let lock =
        file_guard::try_lock(lock_file, lock_type, 0, 1).map_err(|e| LockError::IOError {
            path: path.clone(),
            error: e,
        })?;
    debug!("Lock created: {path:?}");
    Ok(LockInner::Local(path, Some(lock), AtomicBool::new(true)))
}

fn try_create_lock_loop_sync(
    lock_type: file_guard::Lock,
    path: PathBuf,
) -> Result<LockInner, LockError> {
    let start = Instant::now();
    let mut warned = false;
    let mut first = true;
    loop {
        match try_create_lock(lock_type, path.clone()) {
            Ok(lock) => return Ok(lock),
            Err(e) => {
                if first {
                    if let Ok(cmd) = std::fs::read_to_string(&path) {
                        let Ok(cmd) = serde_json::from_str::<serde_json::Value>(&cmd) else {
                            return Err(LockError::BadLockFile { path });
                        };
                        warn!(
                            "Waiting for lock held by process {pid} running {cmd:?}",
                            pid = cmd["pid"].as_u64().unwrap() as u32,
                            cmd = cmd["cmd"].as_str().unwrap().to_string()
                        );
                    }
                    first = false;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                if start.elapsed() > SLOW_LOCK_WARNING {
                    if !warned {
                        warn!("Still waiting for lock ({path:?})");
                        warned = true;
                    }
                }
                if start.elapsed() > SLOW_LOCK_TIMEOUT {
                    if let Ok(cmd) = std::fs::read_to_string(&path) {
                        let Ok(cmd) = serde_json::from_str::<serde_json::Value>(&cmd) else {
                            return Err(LockError::BadLockFile { path });
                        };
                        return Err(LockError::Locked {
                            pid: cmd["pid"].as_u64().unwrap() as u32,
                            cmd: cmd["cmd"].as_str().unwrap().to_string(),
                        });
                    }
                    return Err(LockError::BadLockFile { path });
                }
                continue;
            }
        }
    }
}

async fn try_create_lock_loop_async(
    lock_type: file_guard::Lock,
    path: PathBuf,
) -> Result<LockInner, LockError> {
    let start = Instant::now();
    let mut warned = false;
    let mut first = true;

    loop {
        match try_create_lock(lock_type.clone(), path.clone()) {
            Ok(lock) => return Ok(lock),
            Err(e) => {
                if first {
                    if let Ok(cmd) = std::fs::read_to_string(&path) {
                        let Ok(cmd) = serde_json::from_str::<serde_json::Value>(&cmd) else {
                            return Err(LockError::BadLockFile { path });
                        };
                        warn!(
                            "Waiting for lock held by process {pid} running {cmd:?}",
                            pid = cmd["pid"].as_u64().unwrap() as u32,
                            cmd = cmd["cmd"].as_str().unwrap().to_string()
                        );
                    }
                    first = false;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if start.elapsed() > SLOW_LOCK_WARNING {
                    if !warned {
                        warn!("Still waiting for lock");
                        warned = true;
                    }
                }
                if start.elapsed() > SLOW_LOCK_TIMEOUT {
                    if let Ok(cmd) = std::fs::read_to_string(&path) {
                        let Ok(cmd) = serde_json::from_str::<serde_json::Value>(&cmd) else {
                            return Err(LockError::BadLockFile { path });
                        };
                    }
                }
            }
        }
    }
}

fn instance_lock_path(instance: &InstanceName) -> Option<PathBuf> {
    let instance = instance.clone().into();
    let InstanceName::Local(local_name) = &instance else {
        return None;
    };

    let Some(paths) = gel_tokio::Builder::new()
        .with_system()
        .stored_info()
        .paths()
        .for_instance(&local_name)
    else {
        warn!("Unable to find instance path to lock");
        return None;
    };

    if let Some(parent) = paths.data_dir.parent() {
        _ = std::fs::create_dir_all(parent);
    }
    let mut lock_path = paths.data_dir.clone();
    lock_path.set_extension(LOCK_FILE_NAME);
    Some(lock_path)
}

impl LockManager {
    pub fn lock_instance(instance: &InstanceName) -> Result<InstanceLock, LockError> {
        let Some(lock_path) = instance_lock_path(instance) else {
            return Ok(InstanceLock {
                inner: Arc::new(LockInner::NoLock),
            });
        };

        Ok(InstanceLock {
            inner: Arc::new(try_create_lock_loop_sync(
                file_guard::Lock::Exclusive,
                lock_path,
            )?),
        })
    }

    pub fn lock_read_instance(instance: &InstanceName) -> Result<InstanceLock, LockError> {
        let Some(lock_path) = instance_lock_path(instance) else {
            return Ok(InstanceLock {
                inner: Arc::new(LockInner::NoLock),
            });
        };

        Ok(InstanceLock {
            inner: Arc::new(try_create_lock_loop_sync(
                file_guard::Lock::Shared,
                lock_path,
            )?),
        })
    }

    pub async fn lock_instance_async(instance: &InstanceName) -> Result<InstanceLock, LockError> {
        let Some(lock_path) = instance_lock_path(instance) else {
            return Ok(InstanceLock {
                inner: Arc::new(LockInner::NoLock),
            });
        };

        Ok(InstanceLock {
            inner: Arc::new(
                try_create_lock_loop_async(file_guard::Lock::Exclusive, lock_path).await?,
            ),
        })
    }

    pub async fn lock_read_instance_async(
        instance: &InstanceName,
    ) -> Result<InstanceLock, LockError> {
        let Some(lock_path) = instance_lock_path(instance) else {
            return Ok(InstanceLock {
                inner: Arc::new(LockInner::NoLock),
            });
        };

        Ok(InstanceLock {
            inner: Arc::new(try_create_lock_loop_async(file_guard::Lock::Shared, lock_path).await?),
        })
    }

    pub fn lock_project(path: impl AsRef<Path>) -> Result<ProjectLock, LockError> {
        let Ok(stash_path) = get_stash_path(path.as_ref()) else {
            return Ok(ProjectLock {
                inner: Arc::new(LockInner::NoLock),
            });
        };
        if !stash_path.exists() {
            return Ok(ProjectLock {
                inner: Arc::new(LockInner::NoLock),
            });
        }
        let lock_path = stash_path.join(LOCK_FILE_NAME);
        Ok(ProjectLock {
            inner: Arc::new(try_create_lock_loop_sync(
                file_guard::Lock::Exclusive,
                lock_path,
            )?),
        })
    }

    pub async fn lock_project_async(path: impl AsRef<Path>) -> Result<ProjectLock, LockError> {
        let Ok(stash_path) = get_stash_path(path.as_ref()) else {
            return Ok(ProjectLock {
                inner: Arc::new(LockInner::NoLock),
            });
        };
        if !stash_path.exists() {
            return Ok(ProjectLock {
                inner: Arc::new(LockInner::NoLock),
            });
        }
        let lock_path = stash_path.join(LOCK_FILE_NAME);
        Ok(ProjectLock {
            inner: Arc::new(
                try_create_lock_loop_async(file_guard::Lock::Exclusive, lock_path).await?,
            ),
        })
    }
}
