use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};

use gel_tokio::InstanceName;
use log::{debug, warn};
use serde_json::json;

use crate::portable::project::get_stash_path;

const LOCK_FILE_NAME: &str = "gel-cli.lock";
const SLOW_LOCK_WARNING: Duration = Duration::from_secs(10);
const SLOW_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct InstanceLock {
    #[allow(unused)]
    inner: Arc<LockInner>,
}

impl InstanceLock {}

#[derive(Debug)]
enum LockInner {
    NoLock,
    Local(
        PathBuf,
        Option<file_guard::FileGuard<Box<File>>>,
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
    #[allow(unused)]
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
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .map_err(|e| LockError::IOError {
            path: path.clone(),
            error: e,
        })?;
    let lock_file = Box::new(lock_file);
    let mut lock =
        file_guard::try_lock(lock_file, lock_type, 0, 1).map_err(|e| LockError::IOError {
            path: path.clone(),
            error: e,
        })?;

    lock.set_len(0).map_err(|e| LockError::IOError {
        path: path.clone(),
        error: e,
    })?;
    lock.write_all(
        json!({"pid": std::process::id(), "cmd": std::env::args().collect::<Vec<_>>().join(" ")})
            .to_string()
            .as_bytes(),
    )
    .map_err(|e| LockError::IOError {
        path: path.clone(),
        error: e,
    })?;
    lock.flush().map_err(|e| LockError::IOError {
        path: path.clone(),
        error: e,
    })?;

    debug!("Lock created: {path:?}");
    Ok(LockInner::Local(path, Some(lock), AtomicBool::new(true)))
}

struct LoopState {
    start: Instant,
    warned: bool,
    first: bool,
    path: PathBuf,
}

impl LoopState {
    fn new(path: PathBuf) -> Self {
        Self {
            start: Instant::now(),
            warned: false,
            first: true,
            path,
        }
    }

    fn load_lock_file(&self) -> Result<(u32, String), LockError> {
        let cmd = std::fs::read_to_string(&self.path).map_err(|e| LockError::IOError {
            path: self.path.clone(),
            error: e,
        })?;
        let cmd: serde_json::Value =
            serde_json::from_str(&cmd).map_err(|_| LockError::BadLockFile {
                path: self.path.clone(),
            })?;
        let pid = cmd["pid"].as_u64().ok_or(LockError::BadLockFile {
            path: self.path.clone(),
        })? as u32;
        let cmd = cmd["cmd"]
            .as_str()
            .ok_or(LockError::BadLockFile {
                path: self.path.clone(),
            })?
            .to_string();
        Ok((pid, cmd))
    }

    fn error(&mut self, _e: LockError) -> Result<(), LockError> {
        if self.first {
            self.first = false;
            if let Ok((pid, cmd)) = self.load_lock_file() {
                warn!("Waiting for lock held by process {pid} running {cmd:?}",);
            } else {
                warn!("Waiting for lock ({})", self.path.display());
            }
            self.first = false;
        }
        if self.start.elapsed() > SLOW_LOCK_WARNING {
            if !self.warned {
                if let Ok((pid, cmd)) = self.load_lock_file() {
                    warn!("Still waiting for lock held by process {pid} running {cmd:?}",);
                } else {
                    warn!("Still waiting for lock ({})", self.path.display());
                }
                self.warned = true;
            }
        }
        if self.start.elapsed() > SLOW_LOCK_TIMEOUT {
            if let Ok((pid, cmd)) = self.load_lock_file() {
                return Err(LockError::Locked { pid, cmd });
            }
            return Err(LockError::BadLockFile {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    fn try_create_lock_loop_sync(
        lock_type: file_guard::Lock,
        path: PathBuf,
    ) -> Result<LockInner, LockError> {
        let mut state = LoopState::new(path.clone());
        loop {
            match try_create_lock(lock_type, path.clone()) {
                Ok(lock) => return Ok(lock),
                Err(e) => {
                    state.error(e)?;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                }
            }
        }
    }

    async fn try_create_lock_loop_async(
        lock_type: file_guard::Lock,
        path: PathBuf,
    ) -> Result<LockInner, LockError> {
        let mut state = LoopState::new(path.clone());

        loop {
            match try_create_lock(lock_type.clone(), path.clone()) {
                Ok(lock) => return Ok(lock),
                Err(e) => {
                    state.error(e)?;
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
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
            inner: Arc::new(LoopState::try_create_lock_loop_sync(
                file_guard::Lock::Exclusive,
                lock_path,
            )?),
        })
    }

    #[expect(unused)]
    pub fn lock_read_instance(instance: &InstanceName) -> Result<InstanceLock, LockError> {
        let Some(lock_path) = instance_lock_path(instance) else {
            return Ok(InstanceLock {
                inner: Arc::new(LockInner::NoLock),
            });
        };

        Ok(InstanceLock {
            inner: Arc::new(LoopState::try_create_lock_loop_sync(
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
                LoopState::try_create_lock_loop_async(file_guard::Lock::Exclusive, lock_path)
                    .await?,
            ),
        })
    }

    pub async fn lock_maybe_read_instance_async(
        instance: &InstanceName,
        read_only: bool,
    ) -> Result<InstanceLock, LockError> {
        let Some(lock_path) = instance_lock_path(instance) else {
            return Ok(InstanceLock {
                inner: Arc::new(LockInner::NoLock),
            });
        };

        Ok(InstanceLock {
            inner: Arc::new(
                LoopState::try_create_lock_loop_async(
                    if read_only {
                        file_guard::Lock::Shared
                    } else {
                        file_guard::Lock::Exclusive
                    },
                    lock_path,
                )
                .await?,
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
            inner: Arc::new(
                LoopState::try_create_lock_loop_async(file_guard::Lock::Shared, lock_path).await?,
            ),
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
            inner: Arc::new(LoopState::try_create_lock_loop_sync(
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
                LoopState::try_create_lock_loop_async(file_guard::Lock::Exclusive, lock_path)
                    .await?,
            ),
        })
    }
}
