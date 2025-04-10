use gel_tokio::{Builder, InstanceName, dsn::CredentialsFile};

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
