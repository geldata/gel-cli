use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use gel_tokio::InstanceName;
use log::{debug, warn};
use serde_json::json;

use crate::cli::env::Env;
use crate::portable::project::get_stash_path;

const LOCK_FILE_NAME: &str = "gel-cli.lock";
const SLOW_LOCK_WARNING: Duration = Duration::from_secs(10);
const SLOW_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(500);

static CURRENT_LOCKS: LazyLock<Mutex<HashMap<PathBuf, LockType>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone)]
pub struct InstanceLock {
    #[allow(unused)]
    inner: Arc<LockInner>,
}

#[derive(Debug)]
enum LockInner {
    NoLock(#[expect(unused)] LockType),
    Local(
        LockType,
        PathBuf,
        Option<file_guard::FileGuard<Box<File>>>,
        AtomicBool,
    ),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockType {
    Shared,
    Exclusive,
}

impl Into<file_guard::Lock> for LockType {
    fn into(self) -> file_guard::Lock {
        match self {
            LockType::Shared => file_guard::Lock::Shared,
            LockType::Exclusive => file_guard::Lock::Exclusive,
        }
    }
}

impl Drop for LockInner {
    fn drop(&mut self) {
        if let LockInner::Local(lock_type, path, lock, must_exist) = self {
            CURRENT_LOCKS.lock().unwrap().remove(path);
            if let Some(mut lock) = lock.take() {
                // For a shared lock, try to take an exclusive lock and delete the file if
                // we can.
                debug!("Dropping lock: {path:?}, type: {lock_type:?}");
                if *lock_type == LockType::Shared {
                    #[cfg(unix)]
                    {
                        use file_guard::os::unix::FileGuardExt;
                        if lock.try_upgrade().is_ok() {
                            debug!("Removed shared lock file {path:?}");
                            _ = std::fs::remove_file(&path);
                        }
                    }

                    // On Windows, try to remove the file -- it'll fail if there
                    // are more locks, so ignore the error.
                    if cfg!(windows) {
                        drop(lock);
                        _ = std::fs::remove_file(&path);
                    }
                } else if let Err(e) = std::fs::remove_file(&path) {
                    drop(lock);
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
    #[error("Missing lock file {path:?} ({error})")]
    MissingLockFile {
        path: PathBuf,
        error: Box<LockError>,
    },
    #[error("Failed to lock file {path:?} ({error})")]
    FailedToLock {
        path: PathBuf,
        error: Box<LockError>,
    },
    #[error("I/O error: {error}")]
    IOError {
        path: PathBuf,
        error: std::io::Error,
    },
}

fn try_create_lock(lock_type: LockType, path: PathBuf) -> Result<LockInner, LockError> {
    match try_create_lock_inner(lock_type, &path) {
        Ok(None) => Ok(LockInner::NoLock(lock_type)),
        Ok(Some(lock)) => {
            debug!("{lock_type:?} lock created: {path:?}");
            Ok(LockInner::Local(
                lock_type,
                path,
                Some(lock),
                AtomicBool::new(true),
            ))
        }
        Err(e) => {
            debug!("Failed to create {lock_type:?} lock: {e}");
            Err(LockError::IOError {
                path: path.clone(),
                error: e,
            })
        }
    }
}

fn try_create_lock_inner(
    lock_type: LockType,
    path: &PathBuf,
) -> std::io::Result<Option<file_guard::FileGuard<Box<File>>>> {
    // Special case: we allow "free" shared locks in hooks, since the write lock
    // is presumed to exist in the parent process.
    if lock_type == LockType::Shared && *Env::in_hook().unwrap_or_default().unwrap_or_default() {
        return Ok(None);
    }

    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)?;
    let lock_file = Box::new(lock_file);
    let mut lock = file_guard::try_lock(lock_file, lock_type.into(), 0, 1)?;

    // Once we get the lock, rewrite the file with our data
    if cfg!(windows) {
        // On Windows, accept a failure to write the lockfile data
        if lock.set_len(0).is_ok() {
            _ = lock.write_all(
            json!({"pid": std::process::id(), "cmd": std::env::args().collect::<Vec<_>>().join(" ")})
                .to_string()
                .as_bytes(),
            );
            _ = lock.flush();
        }
    } else {
        lock.set_len(0)?;
        lock.write_all(
        json!({"pid": std::process::id(), "cmd": std::env::args().collect::<Vec<_>>().join(" ")})
            .to_string()
            .as_bytes(),
        )?;
        lock.flush()?;
    }
    Ok(Some(lock))
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

    fn error(&mut self, e: LockError) -> Result<(), LockError> {
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
            if matches!(std::fs::exists(&self.path), Ok(true)) {
                return Err(LockError::FailedToLock {
                    path: self.path.clone(),
                    error: Box::new(e),
                });
            } else {
                return Err(LockError::MissingLockFile {
                    path: self.path.clone(),
                    error: Box::new(e),
                });
            }
        }
        Ok(())
    }

    fn try_create_lock_loop_sync(
        lock_type: LockType,
        path: PathBuf,
    ) -> Result<LockInner, LockError> {
        let mut state = LoopState::new(path.clone());
        loop {
            match try_create_lock(lock_type, path.clone()) {
                Ok(lock) => return Ok(lock),
                Err(e) => {
                    state.error(e)?;
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                    continue;
                }
            }
        }
    }

    async fn try_create_lock_loop_async(
        lock_type: LockType,
        path: PathBuf,
    ) -> Result<LockInner, LockError> {
        let mut state = LoopState::new(path.clone());

        loop {
            match try_create_lock(lock_type, path.clone()) {
                Ok(lock) => return Ok(lock),
                Err(e) => {
                    state.error(e)?;
                    tokio::time::sleep(LOCK_POLL_INTERVAL).await;
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

fn get_existing_lock(path: &PathBuf, lock_type: LockType) -> Option<LockInner> {
    let locks = CURRENT_LOCKS.lock().unwrap();
    if let Some(existing_lock_type) = locks.get(path) {
        if *existing_lock_type == lock_type {
            warn!("{lock_type:?} lock already exists for {path:?}");
        } else {
            warn!("Lock type mismatch for {path:?}: {lock_type:?} != {existing_lock_type:?}");
        }
        return Some(LockInner::NoLock(lock_type));
    }
    None
}

fn lock_instance_sync(
    instance: &InstanceName,
    lock_type: LockType,
) -> Result<InstanceLock, LockError> {
    let Some(lock_path) = instance_lock_path(instance) else {
        return Ok(InstanceLock {
            inner: Arc::new(LockInner::NoLock(lock_type)),
        });
    };

    if let Some(lock) = get_existing_lock(&lock_path, lock_type) {
        return Ok(InstanceLock {
            inner: Arc::new(lock),
        });
    }

    let lock = InstanceLock {
        inner: Arc::new(LoopState::try_create_lock_loop_sync(
            lock_type,
            lock_path.clone(),
        )?),
    };

    CURRENT_LOCKS.lock().unwrap().insert(lock_path, lock_type);
    Ok(lock)
}

async fn lock_instance_async(
    instance: &InstanceName,
    lock_type: LockType,
) -> Result<InstanceLock, LockError> {
    let Some(lock_path) = instance_lock_path(instance) else {
        return Ok(InstanceLock {
            inner: Arc::new(LockInner::NoLock(lock_type)),
        });
    };

    if let Some(lock) = get_existing_lock(&lock_path, lock_type) {
        return Ok(InstanceLock {
            inner: Arc::new(lock),
        });
    }

    let lock = InstanceLock {
        inner: Arc::new(LoopState::try_create_lock_loop_async(lock_type, lock_path.clone()).await?),
    };

    CURRENT_LOCKS.lock().unwrap().insert(lock_path, lock_type);
    Ok(lock)
}

impl LockManager {
    pub fn lock_instance(instance: &InstanceName) -> Result<InstanceLock, LockError> {
        lock_instance_sync(instance, LockType::Exclusive)
    }

    pub async fn lock_instance_async(instance: &InstanceName) -> Result<InstanceLock, LockError> {
        lock_instance_async(instance, LockType::Exclusive).await
    }

    pub async fn lock_maybe_read_instance_async(
        instance: &InstanceName,
        read_only: bool,
    ) -> Result<InstanceLock, LockError> {
        lock_instance_async(
            instance,
            if read_only {
                LockType::Shared
            } else {
                LockType::Exclusive
            },
        )
        .await
    }

    pub fn lock_maybe_read_instance(
        instance: &InstanceName,
        read_only: bool,
    ) -> Result<InstanceLock, LockError> {
        lock_instance_sync(
            instance,
            if read_only {
                LockType::Shared
            } else {
                LockType::Exclusive
            },
        )
    }

    #[expect(unused)]
    pub fn lock_read_instance(instance: &InstanceName) -> Result<InstanceLock, LockError> {
        lock_instance_sync(instance, LockType::Shared)
    }

    pub async fn lock_read_instance_async(
        instance: &InstanceName,
    ) -> Result<InstanceLock, LockError> {
        lock_instance_async(instance, LockType::Shared).await
    }

    pub fn lock_project(path: impl AsRef<Path>) -> Result<ProjectLock, LockError> {
        let Ok(stash_path) = get_stash_path(path.as_ref()) else {
            return Ok(ProjectLock {
                inner: Arc::new(LockInner::NoLock(LockType::Exclusive)),
            });
        };
        if !stash_path.exists() {
            return Ok(ProjectLock {
                inner: Arc::new(LockInner::NoLock(LockType::Exclusive)),
            });
        }
        let lock_path = stash_path.join(LOCK_FILE_NAME);
        Ok(ProjectLock {
            inner: Arc::new(LoopState::try_create_lock_loop_sync(
                LockType::Exclusive,
                lock_path,
            )?),
        })
    }

    pub async fn lock_project_async(path: impl AsRef<Path>) -> Result<ProjectLock, LockError> {
        let Ok(stash_path) = get_stash_path(path.as_ref()) else {
            return Ok(ProjectLock {
                inner: Arc::new(LockInner::NoLock(LockType::Exclusive)),
            });
        };
        if !stash_path.exists() {
            return Ok(ProjectLock {
                inner: Arc::new(LockInner::NoLock(LockType::Exclusive)),
            });
        }
        let lock_path = stash_path.join(LOCK_FILE_NAME);
        Ok(ProjectLock {
            inner: Arc::new(
                LoopState::try_create_lock_loop_async(LockType::Exclusive, lock_path).await?,
            ),
        })
    }
}
