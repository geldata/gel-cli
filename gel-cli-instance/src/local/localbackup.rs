use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use anyhow::bail;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ProcessRunner, Processes, SystemProcessRunner,
    instance::{
        Operation,
        backup::{Backup, BackupId, BackupStrategy, BackupType, InstanceBackup, ProgressCallback},
        map_join_error,
    },
};

use super::LocalInstanceHandle;

const BACKUP_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const BACKUP_LIVENESS_INTERVAL: Duration = Duration::from_secs(60);

pub struct LocalBackup {
    handle: LocalInstanceHandle,
}

impl LocalBackup {
    pub fn new(handle: LocalInstanceHandle) -> Self {
        Self { handle }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct BackupMetadata {
    version: u32,
    #[serde(with = "humantime_serde")]
    started_at: SystemTime,
    #[serde(with = "humantime_serde")]
    last_updated_at: SystemTime,
    #[serde(with = "humantime_serde")]
    completed_at: Option<SystemTime>,
    #[serde(rename = "type")]
    backup_type: BackupType,
    #[serde(rename = "strategy")]
    backup_strategy: BackupStrategy,
    server_version: String,
}

impl InstanceBackup for LocalBackup {
    fn backup(&self, callback: ProgressCallback) -> Operation<Option<BackupId>> {
        let pg_backup = PgBackupCommands::new(SystemProcessRunner, self.handle.bin_dir.clone());
        let backup_id = Uuid::now_v7().to_string();
        let mut backups_dir = self.handle.paths.data_dir.clone();
        backups_dir.set_extension("backups");
        let now = SystemTime::now();
        let mut metadata = BackupMetadata {
            version: 1,
            started_at: now,
            last_updated_at: now,
            completed_at: None,
            backup_type: BackupType::Manual,
            backup_strategy: BackupStrategy::Full,
            server_version: self.handle.version.clone(),
        };

        let target_dir = backups_dir.join(&backup_id);
        let temp_dir = backups_dir.join(format!(".{backup_id}.tmp"));
        let run_dir = self.handle.run_dir.clone();

        tokio::spawn(async move {
            let metadata_file = temp_dir.join("backup.json");
            std::fs::create_dir_all(&temp_dir)?;
            std::fs::write(&metadata_file, serde_json::to_string_pretty(&metadata)?)?;

            // Update the metadata file every minute so we can detect dead
            // backup tasks.
            let task = {
                let metadata_file = metadata_file.clone();
                let mut metadata = metadata.clone();
                tokio::spawn(async move {
                    loop {
                        metadata.last_updated_at = SystemTime::now();
                        let Ok(metadata) = serde_json::to_string_pretty(&metadata) else {
                            return;
                        };
                        _ = std::fs::write(&metadata_file, metadata);
                        tokio::time::sleep(BACKUP_LIVENESS_INTERVAL).await;
                    }
                })
            };

            let task = scopeguard::guard(task, |task| {
                let _ = task.abort();
                // Best effort cleanup
                _ = std::fs::remove_dir_all(&temp_dir);
            });

            pg_backup
                .pg_basebackup(&temp_dir.join("data"), run_dir, "postgres", callback)
                .await?;

            // Abort the task so we don't accidentally race.
            let task = scopeguard::ScopeGuard::into_inner(task);
            task.abort();
            _ = task.await;

            metadata.completed_at = Some(SystemTime::now());
            std::fs::write(metadata_file, serde_json::to_string_pretty(&metadata)?)?;
            std::fs::rename(temp_dir, target_dir)?;
            Ok(Some(BackupId::new(backup_id)))
        })
        .map(map_join_error::<_, anyhow::Error>)
        .boxed()
    }

    fn list_backups(&self) -> Operation<Vec<Backup>> {
        let mut backups_dir = self.handle.paths.data_dir.clone();
        backups_dir.set_extension("backups");

        tokio::task::spawn_blocking(move || {
            let mut backups = vec![];
            let mut to_remove = vec![];
            if !backups_dir.exists() {
                return Ok(backups);
            }
            for entry in std::fs::read_dir(backups_dir)? {
                let Ok(entry) = entry else {
                    continue;
                };
                to_remove.push(entry.path());
                let Ok(uuid) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
                    continue;
                };

                let path = entry.path();
                let metadata_file = path.join("backup.json");
                let Ok(metadata) = std::fs::read_to_string(metadata_file) else {
                    continue;
                };
                let Ok(metadata) = serde_json::from_str::<BackupMetadata>(&metadata) else {
                    continue;
                };

                let elapsed = metadata.last_updated_at.elapsed().unwrap_or(Duration::MAX);
                if elapsed > BACKUP_TIMEOUT {
                    continue;
                }
                to_remove.pop();

                let backup = Backup {
                    id: BackupId::new(uuid.to_string()),
                    created_on: metadata.started_at,
                    status: metadata
                        .completed_at
                        .map_or("in progress".to_string(), |_| "completed".to_string()),
                    backup_type: metadata.backup_type,
                    backup_strategy: metadata.backup_strategy,
                    server_version: metadata.server_version,
                };
                backups.push(backup);
            }
            for path in to_remove {
                log::warn!("Removing incomplete or failed backup {}", path.display());
                _ = std::fs::remove_dir_all(&path);
            }
            Ok(backups)
        })
        .map(map_join_error::<_, anyhow::Error>)
        .boxed()
    }

    fn restore(
        &self,
        _instance: Option<gel_dsn::gel::InstanceName>,
        _restore_type: crate::instance::backup::RestoreType,
        _callback: ProgressCallback,
    ) -> Operation<()> {
        async { bail!("`instance restore` is not yet implemented for local instances") }.boxed()
    }
}

struct PgBackupCommands<P: ProcessRunner> {
    runner: Processes<P>,
    portable_bin_path: PathBuf,
}

impl<P: ProcessRunner> PgBackupCommands<P> {
    pub fn new(runner: P, portable_bin_path: PathBuf) -> Self {
        Self {
            runner: Processes::new(runner),
            portable_bin_path,
        }
    }

    pub async fn pg_basebackup(
        &self,
        target_dir: impl AsRef<Path>,
        unix_path: impl AsRef<Path>,
        username: &str,
        callback: ProgressCallback,
    ) -> Result<(), anyhow::Error> {
        if !self.portable_bin_path.join("pg_basebackup").exists() {
            return Err(anyhow::anyhow!(
                "Backups not supported for this server version. No `pg_basebackup` found at {}.",
                self.portable_bin_path.join("pg_basebackup").display()
            ));
        }
        // TODO: we should trigger the server to start if it's not running
        if !unix_path.as_ref().join(".s.PGSQL.5432").exists() {
            return Err(anyhow::anyhow!(
                "PostgreSQL socket not found at {}. Is the instance sleeping?",
                unix_path.as_ref().join(".s.PGSQL.5432").display()
            ));
        }
        let mut cmd = Command::new(self.portable_bin_path.join("pg_basebackup"));
        cmd.arg("--pgdata").arg(target_dir.as_ref());
        cmd.arg("--host").arg(unix_path.as_ref());
        cmd.arg("--username").arg(username);
        cmd.arg("--format=tar");
        cmd.arg("--checkpoint=fast");
        cmd.arg("--progress");
        cmd.arg("--compress=client-gzip");
        // Slows it down
        // cmd.arg("-r 1000");
        self.runner
            .run_lines(cmd, move |line| {
                callback.progress(None, &format!("running pg_basebackup: {}", line.trim()));
            })
            .await?;
        Ok(())
    }

    // pub async fn pg_restore(&self, callback: ProgressCallback) -> Result<(), ProcessError> {
    //     unimplemented!()
    // }

    // pub async fn pg_combinebackup(&self, callback: ProgressCallback) -> Result<(), ProcessError> {
    //     unimplemented!()
    // }

    // pub async fn pg_verifybackup(&self, callback: ProgressCallback) -> Result<(), ProcessError> {
    //     unimplemented!()
    // }
}
