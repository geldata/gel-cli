use gel_tokio::{Builder, InstanceName, dsn::CredentialsFile};

use anyhow::Context;
use fn_error_context::context;
use fs_err as fs;

use gel_tokio::dsn::CredentialsFile;
use gel_tokio::{Config, InstanceName};

use crate::platform::{config_dir, tmp_file_name};
use crate::question;

pub fn exists(name: &InstanceName) -> anyhow::Result<bool> {
    Ok(read(name)?.is_some())
}

pub fn all_instance_names() -> anyhow::Result<Vec<InstanceName>> {
    Ok(Builder::default().stored_info().credentials().list()?)
}

pub fn read(name: &InstanceName) -> anyhow::Result<Option<CredentialsFile>> {
    Ok(Builder::default()
        .stored_info()
        .credentials()
        .read(name.clone())?)
}

pub fn write(name: &InstanceName, creds: &CredentialsFile) -> anyhow::Result<()> {
    Ok(Builder::default()
        .stored_info()
        .credentials()
        .write(name.clone(), creds)?)
}

pub fn delete(name: &InstanceName) -> anyhow::Result<()> {
    Ok(Builder::default()
        .stored_info()
        .credentials()
        .delete(name.clone())?)
}
