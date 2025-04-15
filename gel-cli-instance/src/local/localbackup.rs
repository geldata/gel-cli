use std::{
    fs::File,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use anyhow::bail;
use futures::FutureExt;
use gel_dsn::gel::InstanceName;
use log::info;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ProcessRunner, Processes, SystemProcessRunner,
    instance::{
        Operation,
        backup::{
            Backup, BackupId, BackupStrategy, BackupType, InstanceBackup, ProgressCallback,
            RestoreType,
        },
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

impl BackupMetadata {
    fn is_incomplete(&self) -> bool {
        self.completed_at.is_none()
            && self.last_updated_at.elapsed().unwrap_or(Duration::MAX) > BACKUP_TIMEOUT
    }

    fn from_file(path: impl AsRef<Path>) -> Result<Self, anyhow::Error> {
        let file = File::open(path.as_ref())?;
        let metadata: Self = serde_json::from_reader(file)?;

        if metadata.completed_at.is_none()
            && metadata.last_updated_at.elapsed().unwrap_or(Duration::MAX) > BACKUP_TIMEOUT
        {
            return Err(anyhow::anyhow!(
                "Backup {} timed out.",
                path.as_ref().display()
            ));
        }

        Ok(metadata)
    }
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
        let run_dir = self.handle.paths.runstate_path.clone();

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

            // TODO: we should trigger the server to start if it's not running
            if !run_dir.join(".s.PGSQL.5432").exists() {
                return Err(anyhow::anyhow!(
                    "PostgreSQL socket not found at {}. Is the instance stopped or sleeping?",
                    run_dir.join(".s.PGSQL.5432").display()
                ));
            }

            pg_backup
                .pg_basebackup(&temp_dir.join("data"), run_dir, "postgres", callback)
                .await?;

            // Abort the task so we don't accidentally race.
            let task = scopeguard::ScopeGuard::into_inner(task);
            task.abort();
            _ = task.await;

            metadata.last_updated_at = SystemTime::now();
            metadata.completed_at = Some(metadata.last_updated_at);
            std::fs::write(metadata_file, serde_json::to_string_pretty(&metadata)?)?;
            std::fs::rename(temp_dir, target_dir)?;
            Ok(Some(BackupId::new(backup_id)))
        })
        .map(map_join_error::<_, anyhow::Error>)
        .boxed()
    }

    fn restore(
        &self,
        instance: Option<InstanceName>,
        restore_type: RestoreType,
        callback: ProgressCallback,
    ) -> Operation<()> {
        let pg_backup = PgBackupCommands::new(SystemProcessRunner, self.handle.bin_dir.clone());
        let mut backups_dir = self.handle.paths.data_dir.clone();
        backups_dir.set_extension("backups");

        let data_dir = self.handle.paths.data_dir.clone();

        let mut restore_tmpdir = self.handle.paths.data_dir.clone();
        restore_tmpdir.set_file_name(".restore.new_data.tmp");

        let mut restore_tmpdir2 = self.handle.paths.data_dir.clone();
        restore_tmpdir2.set_file_name(".restore.old_data.tmp");
        let run_dir = self.handle.paths.runstate_path.clone();

        async move {
            if restore_tmpdir.exists() {
                let restore_tmpdir = restore_tmpdir.clone();
                callback.progress(None, "Removing existing restore directory...");
                tokio::task::spawn_blocking(move || {
                    std::fs::remove_dir_all(&restore_tmpdir)?;
                    Ok::<_, std::io::Error>(())
                }).await??;
            }
            if restore_tmpdir2.exists() {
                bail!("Restore directory {} already exists from an incomplete restore, please remove it manually.", restore_tmpdir2.display());
            }

            if run_dir.join(".s.PGSQL.5432").exists() {
                return Err(anyhow::anyhow!(
                    "PostgreSQL socket found at {}. Stop the server before restoring.",
                    run_dir.join(".s.PGSQL.5432").display()
                ));
            }

            if instance.is_some() {
                bail!("`instance restore` from another instance is not yet implemented.")
            }
            let backup_id = match restore_type {
                RestoreType::Latest => {
                    let mut latest = Uuid::nil();
                    for entry in std::fs::read_dir(&backups_dir).into_iter().flatten().flatten() {
                        let Ok(uuid) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
                            continue;
                        };
                        // UUID v7 is monotonically increasing, so we can just
                        // compare the values.
                        if uuid > latest {
                            latest = uuid;
                        }
                    }
                    BackupId::new(latest.to_string())
                }
                RestoreType::Specific(id) => {
                    BackupId::new(id)
                }
            };

            info!("Restoring backup {backup_id} from {backups_dir:?}");

            let backup_path = backups_dir.join(PathBuf::from(backup_id.to_string()));
            if !backup_path.try_exists()? {
                bail!("Backup {} not found", backup_id);
            }

            let metadata_file = backup_path.join("backup.json");
            let metadata = BackupMetadata::from_file(&metadata_file)?;
            if metadata.is_incomplete() {
                bail!("Backup {} is incomplete and cannot be restored.", backup_id);
            }

            let backup_data_path = backup_path.join("data");
            let data_file = backup_data_path.join("base.tar.gz");
            let wal_file = backup_data_path.join("pg_wal.tar.gz");
            let backup_manifest = backup_data_path.join("backup_manifest");
            {
                let callback = callback.clone();
                let restore_tmpdir = restore_tmpdir.clone();
                tokio::task::spawn_blocking(move || {
                    // Note to reader: tar files may contain multiple
                    // concatenated gzip streams, so we need to use a
                    // MultiGzDecoder to unpack them.

                    info!("Unpacking {data_file:?} to {restore_tmpdir:?}");
                    callback.progress(None, "Unpacking data...");
                    let mut data_tar = tar::Archive::new(flate2::read::MultiGzDecoder::new(File::open(data_file)?));
                    data_tar.unpack(&restore_tmpdir)?;

                    let wal_dir = restore_tmpdir.join("pg_wal");
                    info!("Unpacking {wal_file:?} to {wal_dir:?}");
                    callback.progress(None, "Unpacking WAL...");
                    let mut wal_tar = tar::Archive::new(flate2::read::MultiGzDecoder::new(File::open(wal_file)?));
                    wal_tar.unpack(wal_dir)?;

                    Ok::<_, std::io::Error>(())
                }).await??;
            }
            pg_backup.pg_verifybackup(&restore_tmpdir, backup_manifest, callback.clone()).await?;

            callback.progress(None, "Finalizing restore...");

            tokio::task::spawn_blocking(move || {
                std::fs::rename(&data_dir, &restore_tmpdir2)?;
                #[cfg(unix)] {
                    use std::os::unix::fs::PermissionsExt                    ;
                    std::fs::set_permissions(&restore_tmpdir, std::fs::Permissions::from_mode(0o700))?;
                }
                std::fs::rename(&restore_tmpdir, &data_dir)?;
                std::fs::remove_dir_all(&restore_tmpdir2)?;
                Ok::<_, std::io::Error>(())
            }).await??;

            Ok(())
        }.boxed()
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
                let Ok(metadata) = BackupMetadata::from_file(&metadata_file) else {
                    continue;
                };
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

    pub async fn pg_verifybackup(
        &self,
        backup_dir: impl AsRef<Path>,
        backup_manifest: impl AsRef<Path>,
        callback: ProgressCallback,
    ) -> Result<(), anyhow::Error> {
        if !self.portable_bin_path.join("pg_verifybackup").exists() {
            return Err(anyhow::anyhow!(
                "Restores not supported for this server version. No `pg_verifybackup` found at {}.",
                self.portable_bin_path.join("pg_verifybackup").display()
            ));
        }
        let mut cmd = Command::new(self.portable_bin_path.join("pg_verifybackup"));
        cmd.arg("--progress");
        cmd.arg("--manifest-path").arg(backup_manifest.as_ref());
        cmd.arg(backup_dir.as_ref());
        self.runner
            .run_lines(cmd, move |line| {
                callback.progress(None, &format!("running pg_verifybackup: {}", line.trim()));
            })
            .await?;
        Ok(())
    }
}
