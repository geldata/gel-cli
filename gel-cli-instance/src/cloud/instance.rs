use futures::FutureExt;
use gel_dsn::gel::{CloudName, InstanceName};
use std::time::Duration;

use crate::instance::{
    Instance, InstanceOpError, Operation,
    backup::{
        Backup, BackupId, BackupStrategy, BackupType, InstanceBackup, ProgressCallback,
        RequestedBackupStrategy, RestoreType,
    },
    map_join_error,
};

use super::{CloudApi, CloudError, CloudHttp, schema};

#[derive(Debug, Clone)]
pub struct CloudInstanceHandle<H: CloudHttp> {
    pub name: CloudName,
    pub api: CloudApi<H>,
}

struct CloudInstanceBackup<H: CloudHttp> {
    instance: CloudInstanceHandle<H>,
}

impl<H: CloudHttp> InstanceBackup for CloudInstanceBackup<H> {
    fn backup(
        &self,
        strategy: RequestedBackupStrategy,
        callback: ProgressCallback,
    ) -> Operation<Option<BackupId>> {
        let api = self.instance.api.clone();
        let name = self.instance.name.clone();

        tokio::spawn(async move {
            if strategy != RequestedBackupStrategy::Auto {
                return Err(CloudError::InvalidRequest(
                    "cloud backups only support automatic backups".to_string(),
                ));
            }

            let operation = api.create_backup(&name).await?;
            api.wait_for_operation(operation, Duration::from_secs(30 * 60), callback)
                .await?;
            Ok(None)
        })
        .map(map_join_error::<_, CloudError>)
        .boxed()
    }

    fn restore(
        &self,
        instance: Option<InstanceName>,
        restore_type: RestoreType,
        callback: ProgressCallback,
    ) -> Operation<()> {
        let api = self.instance.api.clone();
        let name = self.instance.name.clone();

        callback.progress(
            None,
            &format!("preparing to restore \"{}\"", self.instance.name),
        );

        tokio::spawn(async move {
            let cloud_name = match instance {
                Some(InstanceName::Cloud(name)) => Some(name),
                None => None,
                _ => {
                    return Err(CloudError::InvalidRequest(
                        "source instance must also be a cloud instance".to_string(),
                    ));
                }
            };

            let source_instance_id = if let Some(name) = cloud_name {
                Some(api.get_instance(&name).await?.id)
            } else {
                None
            };

            let request = match restore_type {
                RestoreType::Latest => schema::CloudInstanceRestore {
                    latest: true,
                    source_instance_id,
                    ..Default::default()
                },
                RestoreType::Specific(id) => schema::CloudInstanceRestore {
                    backup_id: Some(id),
                    source_instance_id,
                    ..Default::default()
                },
            };

            let operation = api.restore_instance(&name, request).await?;
            api.wait_for_operation(operation, Duration::from_secs(30 * 60), callback)
                .await?;
            Ok(())
        })
        .map(map_join_error)
        .boxed()
    }

    fn list_backups(&self) -> Operation<Vec<Backup>> {
        let api = self.instance.api.clone();
        let name = self.instance.name.clone();

        tokio::spawn(async move {
            let backups = api.list_backups(&name).await?;
            Ok(backups
                .into_iter()
                .map(|b| Backup {
                    id: BackupId::new(b.id),
                    created_on: b.created_on,
                    status: b.status,
                    backup_type: match b.r#type.as_str().to_lowercase().as_str() {
                        "automated" => BackupType::Automated,
                        "on-demand" => BackupType::Manual,
                        _ => BackupType::Unknown(b.r#type),
                    },
                    server_version: b.edgedb_version,
                    backup_strategy: BackupStrategy::Full,
                    size: None,
                    location: None,
                })
                .collect())
        })
        .map(map_join_error::<_, CloudError>)
        .boxed()
    }

    fn get_backup(&self, _backup_id: &BackupId) -> anyhow::Result<Backup> {
        todo!()
    }
}

impl<H: CloudHttp> Instance for CloudInstanceHandle<H> {
    fn backup(&self) -> Result<Box<dyn InstanceBackup + Send>, InstanceOpError> {
        Ok(Box::new(CloudInstanceBackup {
            instance: self.clone(),
        }))
    }
}
