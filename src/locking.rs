use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use gel_tokio::{InstanceName, dsn::StoredInformation};
use log::warn;
use serde_json::json;

struct Lock {}

#[derive(Debug, Clone)]
pub struct InstanceLock {
    inner: Arc<InstanceLockInner>,
}

#[derive(Debug)]
enum InstanceLockInner {
    NoLock,
    Local(PathBuf, Option<file_guard::FileGuard<Arc<File>>>),
}

impl Drop for InstanceLockInner {
    fn drop(&mut self) {
        if let InstanceLockInner::Local(path, lock) = self {
            if let Some(lock) = lock.take() {
                drop(lock);
                if let Err(e) = std::fs::remove_file(&path) {
                    warn!("Failed to remove lock file {path:?}: {e}");
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectLock {}

pub struct LockManager {}

#[derive(thiserror::Error, Debug)]
pub enum LockError {}

struct LockManagerInstance {}

impl LockManagerInstance {
    pub fn new() {}
}

fn try_create_lock(path: PathBuf) -> Result<InstanceLockInner, LockError> {
    let mut lock_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .unwrap();
    lock_file
        .write_all(
            json!({"pid": std::process::id(), "cmd": std::env::args().collect::<Vec<_>>()})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let lock_file = Arc::new(lock_file);
    let lock = file_guard::lock(lock_file, file_guard::Lock::Exclusive, 0, 1).unwrap();
    Ok(InstanceLockInner::Local(path, Some(lock)))
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

    std::fs::create_dir_all(&paths.runstate_path).unwrap();
    let lock_path = paths.runstate_path.join("lock.cli");
    Some(lock_path)
}

impl LockManager {
    pub fn lock_instance(instance: &InstanceName) -> Result<InstanceLock, LockError> {
        let Some(lock_path) = instance_lock_path(instance) else {
            return Ok(InstanceLock {
                inner: Arc::new(InstanceLockInner::NoLock),
            });
        };

        loop {
            match try_create_lock(lock_path.clone()) {
                Ok(lock) => {
                    return Ok(InstanceLock {
                        inner: Arc::new(lock),
                    });
                }
                Err(e) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                }
            }
        }
    }

    pub fn lock_read_instance(
        instance: &impl Into<InstanceName>,
    ) -> Result<InstanceLock, LockError> {
        Ok(InstanceLock {
            inner: Arc::new(InstanceLockInner::NoLock),
        })
    }

    pub async fn lock_instance_async(instance: &InstanceName) -> Result<InstanceLock, LockError> {
        let Some(lock_path) = instance_lock_path(instance) else {
            return Ok(InstanceLock {
                inner: Arc::new(InstanceLockInner::NoLock),
            });
        };

        loop {
            match try_create_lock(lock_path.clone()) {
                Ok(lock) => {
                    return Ok(InstanceLock {
                        inner: Arc::new(lock),
                    });
                }
                Err(e) => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            }
        }
    }

    pub async fn lock_read_instance_async(
        instance: &impl Into<InstanceName>,
    ) -> Result<InstanceLock, LockError> {
        Ok(InstanceLock {
            inner: Arc::new(InstanceLockInner::NoLock),
        })
    }

    pub fn lock_project(path: impl AsRef<Path>) -> Result<ProjectLock, LockError> {
        Ok(ProjectLock {})
    }

    pub async fn lock_project_async(path: impl AsRef<Path>) -> Result<ProjectLock, LockError> {
        Ok(ProjectLock {})
    }
}
