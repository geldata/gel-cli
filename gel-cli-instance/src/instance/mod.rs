use crate::{
    cloud::{CloudApi, CloudHttp, CloudInstanceHandle},
    local::LocalInstanceHandle,
};
use gel_dsn::gel::CloudName;
use std::pin::Pin;

pub mod backup;

pub type Operation<T> = Pin<Box<dyn Future<Output = Result<T, anyhow::Error>>>>;

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

pub fn get_local_instance(name: &str) -> Result<InstanceHandle, InstanceOpError> {
    let instance = LocalInstanceHandle {
        name: name.to_string(),
    };
    Ok(InstanceHandle {
        instance: Box::new(instance),
    })
}
