use std::{sync::Arc, time::SystemTime};

use gel_dsn::gel::InstanceName;
use serde::Serialize;

use super::Operation;

pub type Error = anyhow::Error;

pub trait ProgressCallbackListener: Send + Sync + 'static {
    fn progress(&self, progress: Option<f64>, message: &str);
}

#[derive(Clone)]
pub struct ProgressCallback {
    listener: Arc<dyn ProgressCallbackListener>,
}

impl<T: ProgressCallbackListener> From<T> for ProgressCallback {
    fn from(listener: T) -> Self {
        Self {
            listener: Arc::new(listener),
        }
    }
}

impl std::ops::Deref for ProgressCallback {
    type Target = dyn ProgressCallbackListener;

    fn deref(&self) -> &Self::Target {
        self.listener.deref()
    }
}

#[derive(Debug, Copy, Clone, derive_more::Display, Serialize)]
pub enum BackupType {
    Automated,
    Manual,
}

#[derive(Debug, Clone)]
pub enum RestoreType {
    Latest,
    Specific(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, derive_more::Display, Serialize)]
#[display("{}", id)]
pub struct BackupId {
    id: String,
}

impl BackupId {
    pub fn new(id: String) -> Self {
        Self { id }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Backup {
    pub id: BackupId,
    pub created_on: SystemTime,
    pub backup_type: BackupType,
    pub status: String,
    pub server_version: String,
}

pub trait InstanceBackup {
    /// Perform a backup. Returns the backup id if available.
    fn backup(&self, callback: ProgressCallback) -> Operation<Option<BackupId>>;
    /// Restore from a backup, optionally from a different instance.
    fn restore(
        &self,
        instance: Option<InstanceName>,
        restore_type: RestoreType,
        callback: ProgressCallback,
    ) -> Operation<()>;
    /// List backups.
    fn list_backups(&self) -> Operation<Vec<Backup>>;
}
