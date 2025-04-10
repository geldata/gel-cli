use std::{path::PathBuf, sync::Arc};

use localbackup::LocalBackup;

use crate::instance::{Instance, InstanceOpError, backup::InstanceBackup};
use gel_dsn::gel::InstancePaths;

mod localbackup;

#[derive(Debug, Clone)]
pub struct LocalInstanceHandle {
    pub name: String,
    pub paths: Arc<InstancePaths>,
    pub bin_dir: PathBuf,
    pub run_dir: PathBuf,
    pub version: String,
}

impl Instance for LocalInstanceHandle {
    fn backup(&self) -> Result<Box<dyn InstanceBackup>, InstanceOpError> {
        Ok(Box::new(LocalBackup::new(self.clone())))
    }
}
