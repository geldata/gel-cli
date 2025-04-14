use crate::{
    cloud::{CloudApi, CloudHttp, CloudInstanceHandle},
    local::LocalInstanceHandle,
};
use gel_dsn::gel::{Builder, CloudName};
use std::{path::PathBuf, pin::Pin, sync::Arc};
use tokio::task::JoinError;

pub mod backup;

pub type Operation<T> = Pin<Box<dyn Future<Output = Result<T, anyhow::Error>>>>;

pub fn map_join_error<T, E: Into<anyhow::Error>>(
    result: Result<Result<T, E>, JoinError>,
) -> Result<T, anyhow::Error> {
    match result {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(e)) => Err(e.into()),
        Err(e) => Err(e.into()),
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InstanceOpError {
    #[error("Unsupported operation for {0} instances")]
    Unsupported(String),
}

pub trait Instance {
    /// Returns a backup interface for the instance, or an error if the instance
    /// does not support backups.
    fn backup(&self) -> Result<Box<dyn backup::InstanceBackup>, InstanceOpError>;
}

pub struct InstanceHandle {
    instance: Box<dyn Instance>,
}

impl std::ops::Deref for InstanceHandle {
    type Target = dyn Instance;

    fn deref(&self) -> &Self::Target {
        self.instance.deref()
    }
}

pub fn get_cloud_instance<H: CloudHttp>(
    name: CloudName,
    api: CloudApi<H>,
) -> Result<InstanceHandle, InstanceOpError> {
    let instance = CloudInstanceHandle { name, api };
    Ok(InstanceHandle {
        instance: Box::new(instance),
    })
}

pub fn get_local_instance(
    name: &str,
    bin_dir: PathBuf,
    version: String,
) -> Result<InstanceHandle, InstanceOpError> {
    let paths = Builder::default().with_system().stored_info().paths();
    let instance_paths = paths
        .for_instance(name)
        .ok_or_else(|| InstanceOpError::Unsupported(name.to_string()))?;
    let instance: LocalInstanceHandle = LocalInstanceHandle {
        name: name.to_string(),
        paths: Arc::new(instance_paths),
        bin_dir,
        version,
    };
    Ok(InstanceHandle {
        instance: Box::new(instance),
    })
}
