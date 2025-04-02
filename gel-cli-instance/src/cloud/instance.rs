use futures::FutureExt;
use gel_dsn::gel::{CloudName, InstanceName};
use std::time::Duration;
use tokio::task::JoinError;

use crate::instance::{
    Instance, InstanceOpError, Operation,
    backup::{Backup, BackupId, BackupType, Error, InstanceBackup, ProgressCallback, RestoreType},
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

fn map_join_error<T>(result: Result<Result<T, CloudError>, JoinError>) -> Result<T, Error> {
    match result {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(e)) => Err(e.into()),
        Err(e) => Err(e.into()),
    }
}

impl<H: CloudHttp> InstanceBackup for CloudInstanceBackup<H> {
    fn backup(&self, callback: ProgressCallback) -> Operation<Option<BackupId>> {
        let api = self.instance.api.clone();
        let name = self.instance.name.clone();

        tokio::spawn(async move {
            let operation = api
                .create_backup(name.org_slug.as_str(), name.name.as_str())
                .await?;
            api.wait_for_operation(operation, Duration::from_secs(30 * 60), callback)
                .await?;
            Ok(None)
        })
        .map(map_join_error)
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
                Some(api.get_instance(&name.org_slug, &name.name).await?.id)
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

            let operation = api
                .restore_instance(name.org_slug.as_str(), name.name.as_str(), request)
                .await?;
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
            let backups = api
                .list_backups(name.org_slug.as_str(), name.name.as_str())
                .await?;
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
                })
                .collect())
        })
        .map(map_join_error)
        .boxed()
    }
}

impl<H: CloudHttp> Instance for CloudInstanceHandle<H> {
    fn backup(&self) -> Result<Box<dyn InstanceBackup>, InstanceOpError> {
        Ok(Box::new(CloudInstanceBackup {
            instance: self.clone(),
        }))
    }
}
