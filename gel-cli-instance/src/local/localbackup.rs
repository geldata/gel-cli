use std::{
    fs::File,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use anyhow::bail;
use futures::FutureExt;
use gel_dsn::gel::InstanceName;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    ProcessRunner, Processes, SystemProcessRunner,
    instance::{
        Operation,
        backup::{
            Backup, BackupId, BackupStrategy, BackupType, InstanceBackup, ProgressCallback,
            RequestedBackupStrategy, RestoreType,
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

    fn get_backups_dir(&self) -> PathBuf {
        let mut backups_dir = self.handle.paths.data_dir.clone();
        backups_dir.set_extension("backups");
        backups_dir
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(rename = "type")]
    backup_type: BackupType,
    #[serde(rename = "strategy")]
    backup_strategy: BackupStrategy,
    #[serde(skip_serializing_if = "Option::is_none")]
    incremental: Option<IncrementalMetadata>,
    /// The size of the backup in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    server_version: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct IncrementalMetadata {
    #[serde(rename = "parent")]
    parent_backup_id: BackupId,
    #[serde(rename = "generation", default = "u32::default")]
    incremental_generation: u32,
    #[serde(with = "humantime_serde")]
    full_backup_completed_at: SystemTime,
}

const MAX_INCREMENTAL_AGE: Duration = Duration::from_secs(60 * 60 * 24 * 7);
const MAX_INCREMENTAL_GENERATION: u32 = 5;

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

    fn into_backup(self, id: BackupId, location: Option<String>) -> Backup {
        Backup {
            id,
            created_on: self.started_at,
            status: self
                .completed_at
                .map_or("in progress".to_string(), |_| "completed".to_string()),
            backup_type: self.backup_type,
            backup_strategy: self.backup_strategy,
            server_version: self.server_version,
            size: self.size,
            location,
        }
    }
}

struct BackupRecord {
    id: BackupId,
    metadata: BackupMetadata,
    metadata_dir: PathBuf,
    data_dir: PathBuf,
}

impl BackupRecord {
    fn from_file(backups_dir: impl AsRef<Path>, id: BackupId) -> Result<Self, anyhow::Error> {
        let metadata_dir = backups_dir.as_ref().join(id.to_string());
        let file = metadata_dir.join("backup.json");
        let metadata = BackupMetadata::from_file(&file)?;
        let data_dir = metadata_dir.join("data");
        Ok(Self {
            id,
            metadata,
            metadata_dir,
            data_dir,
        })
    }

    fn latest(backups_dir: impl AsRef<Path>) -> Result<Option<BackupId>, anyhow::Error> {
        // Try to read the latest backup id, if it exists.
        if let Ok(id) = std::fs::read_to_string(backups_dir.as_ref().join("latest")) {
            if let Ok(id) = Uuid::parse_str(&id) {
                return Ok(Some(BackupId::new(id.to_string())));
            }
        }

        let read_dir = match std::fs::read_dir(&backups_dir) {
            Ok(read_dir) => read_dir,
            Err(e) => {
                use anyhow::Context;
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(e).context("error reading backups directory")?;
            }
        };

        let mut latest = Uuid::nil();
        for entry in read_dir {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    warn!(
                        "unexpected error reading backups directory, skipping ahead: {}",
                        e
                    );
                    continue;
                }
            };
            let Ok(uuid) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            // UUID v7 is monotonically increasing, so we can just
            // compare the values.
            if uuid > latest {
                let Ok(metadata) = BackupMetadata::from_file(
                    backups_dir
                        .as_ref()
                        .join(entry.file_name())
                        .join("backup.json"),
                ) else {
                    continue;
                };
                if metadata.completed_at.is_some() {
                    latest = uuid;
                }
            }
        }
        if latest.is_nil() {
            Ok(None)
        } else {
            Ok(Some(BackupId::new(latest.to_string())))
        }
    }

    async fn latest_async(
        backups_dir: impl AsRef<Path>,
    ) -> Result<Option<BackupId>, anyhow::Error> {
        let backups_dir = backups_dir.as_ref().to_path_buf();
        let latest = tokio::task::spawn_blocking(move || Self::latest(&backups_dir)).await??;
        Ok(latest)
    }
}

impl InstanceBackup for LocalBackup {
    fn backup(
        &self,
        strategy: RequestedBackupStrategy,
        callback: ProgressCallback,
    ) -> Operation<Option<BackupId>> {
        let pg_backup = PgBackupCommands::new(SystemProcessRunner, self.handle.bin_dir.clone());
        let backup_id = Uuid::now_v7().to_string();
        let backups_dir = self.get_backups_dir();
        let now = SystemTime::now();
        let our_pid = std::process::id();

        let mut metadata = BackupMetadata {
            version: 1,
            started_at: now,
            last_updated_at: now,
            completed_at: None,
            pid: Some(our_pid),
            incremental: None,
            backup_type: BackupType::Manual,
            backup_strategy: BackupStrategy::Full,
            server_version: self.handle.version.clone(),
            size: None,
        };

        let target_dir = backups_dir.join(&backup_id);
        let latest_backup = backups_dir.join("latest");
        let latest_backup_temp = backups_dir.join(".latest.tmp");
        let temp_dir = backups_dir.join(format!(".{backup_id}.tmp"));
        let run_dir = self.handle.paths.runstate_path.clone();

        tokio::spawn(async move {
            let incremental_parent = if strategy == RequestedBackupStrategy::Incremental
                || strategy == RequestedBackupStrategy::Auto
            {
                let latest_backup_id = BackupRecord::latest_async(&backups_dir).await?;
                if latest_backup_id.is_none() && strategy == RequestedBackupStrategy::Incremental {
                    bail!("No previous backup found, cannot take incremental backup.");
                }

                let mut record = latest_backup_id
                    .map(|id| BackupRecord::from_file(backups_dir, id))
                    .transpose()?;
                if strategy == RequestedBackupStrategy::Auto {
                    if let Some(parent_record) = &record {
                        if parent_record.metadata.completed_at.is_none() {
                            record = None;
                        } else if let Some(incremental) = &parent_record.metadata.incremental {
                            if (incremental.incremental_generation > MAX_INCREMENTAL_GENERATION)
                                || incremental
                                    .full_backup_completed_at
                                    .elapsed()
                                    .unwrap_or(Duration::MAX)
                                    > MAX_INCREMENTAL_AGE
                            {
                                record = None;
                            }
                        }
                    } else {
                        record = None;
                    }
                }
                record
            } else {
                None
            };

            metadata.backup_strategy = if incremental_parent.is_some() {
                BackupStrategy::Incremental
            } else {
                BackupStrategy::Full
            };

            // Write a child record so the parent cannot be deleted.
            if let Some(parent_record) = &incremental_parent {
                std::fs::write(
                    parent_record
                        .metadata_dir
                        .join(format!("{}.child", backup_id)),
                    json!({"status": "in-progress", "pid": our_pid}).to_string(),
                )?;
            }

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
                task.abort();
                // Best effort cleanup
                _ = std::fs::remove_dir_all(&temp_dir);
            });

            // This shouldn't happen. We should have attempted to start the
            // server before calling this method, but be clear just in case.
            if !run_dir.join(".s.PGSQL.5432").exists() {
                return Err(anyhow::anyhow!(
                    "PostgreSQL socket not found at {}. Is the instance stopped or sleeping?",
                    run_dir.join(".s.PGSQL.5432").display()
                ));
            }

            if let Some(parent_record) = &incremental_parent {
                metadata.incremental = Some(IncrementalMetadata {
                    parent_backup_id: parent_record.id.clone(),
                    incremental_generation: parent_record
                        .metadata
                        .incremental
                        .as_ref()
                        .map_or(0, |i| i.incremental_generation)
                        + 1,
                    full_backup_completed_at: parent_record
                        .metadata
                        .incremental
                        .as_ref()
                        .map_or(parent_record.metadata.completed_at.unwrap(), |i| {
                            i.full_backup_completed_at
                        }),
                });
                pg_backup
                    .pg_basebackup_incremental(
                        &temp_dir.join("data"),
                        run_dir,
                        "postgres",
                        parent_record.data_dir.join("backup_manifest"),
                        callback,
                    )
                    .await?;
            } else {
                pg_backup
                    .pg_basebackup(&temp_dir.join("data"), run_dir, "postgres", callback)
                    .await?;
            }

            // Abort the task so we don't accidentally race.
            let task = scopeguard::ScopeGuard::into_inner(task);
            task.abort();
            _ = task.await;

            metadata.last_updated_at = SystemTime::now();
            metadata.pid = None;
            metadata.completed_at = Some(metadata.last_updated_at);
            let mut size = 0;
            for entry in std::fs::read_dir(temp_dir.join("data"))? {
                let Ok(entry) = entry else {
                    continue;
                };
                size += entry.metadata()?.len();
            }
            metadata.size = Some(size);

            std::fs::write(metadata_file, serde_json::to_string_pretty(&metadata)?)?;
            std::fs::rename(temp_dir, target_dir)?;

            if let Some(parent_record) = &incremental_parent {
                std::fs::write(
                    parent_record
                        .metadata_dir
                        .join(format!("{}.child", backup_id)),
                    json!({"status": "finalizing", "pid": our_pid}).to_string(),
                )?;
            }

            // Update the latest backup file atomically, if possible.
            _ = std::fs::remove_file(&latest_backup_temp);
            std::fs::write(&latest_backup_temp, backup_id.as_bytes())?;
            if std::fs::rename(&latest_backup_temp, &latest_backup).is_err() {
                std::fs::remove_file(&latest_backup)?;
                std::fs::write(&latest_backup_temp, backup_id.as_bytes())?;
                std::fs::rename(&latest_backup_temp, &latest_backup)?;
            }

            if let Some(parent_record) = &incremental_parent {
                std::fs::write(
                    parent_record
                        .metadata_dir
                        .join(format!("{}.child", backup_id)),
                    json!({"status": "completed"}).to_string(),
                )?;
            }

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
        let backups_dir = self.get_backups_dir();

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
                    let latest_backup_id = BackupRecord::latest_async(&backups_dir).await?;
                    if let Some(latest_backup_id) = latest_backup_id {
                        latest_backup_id
                    } else {
                        bail!("No backups were found.");
                    }
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

            let record = BackupRecord::from_file(&backups_dir, backup_id)?;
            if record.metadata.is_incomplete() {
                bail!("Backup {} is incomplete and cannot be restored.", record.id);
            }

            if record.metadata.backup_strategy == BackupStrategy::Full {
                let backup_data_path = backup_path.join("data");
                let backup_manifest = backup_data_path.join("backup_manifest");
                pg_backup.unpack_backup(&backup_data_path, &restore_tmpdir, callback.clone()).await?;
                pg_backup.pg_verifybackup(&restore_tmpdir, backup_manifest, callback.clone()).await?;
            } else if record.metadata.backup_strategy == BackupStrategy::Incremental {
                // Walk the backup chain to the full backup.
                let Some(mut current) = record.metadata.incremental.as_ref().map(|i| i.parent_backup_id.clone()) else {
                    return Err(anyhow::anyhow!("Backup {} is corrupt: missing incremental metadata.", record.id));
                };
                let mut backup_chain = vec![record];
                loop {
                    let record = BackupRecord::from_file(&backups_dir, current)?;
                    if record.metadata.backup_strategy == BackupStrategy::Full {
                        backup_chain.push(record);
                        break;
                    } else {
                        let Some(incremental) = &record.metadata.incremental else {
                            bail!("Backup {} is corrupt: missing incremental metadata.", record.id);
                        };
                        current = incremental.parent_backup_id.clone();
                    }
                    backup_chain.push(record);
                }

                backup_chain.reverse();

                let mut combine_backup_args = vec![];

                for record in backup_chain {
                    let target = restore_tmpdir2.join(record.id.to_string());
                    pg_backup.unpack_backup(&record.data_dir, &target, callback.clone()).await?;
                    combine_backup_args.push(target);
                }

                pg_backup.pg_combinebackup(&restore_tmpdir, &combine_backup_args, callback.clone()).await?;
                {
                    let restore_tmpdir2 = restore_tmpdir2.clone();
                    tokio::task::spawn_blocking(move || {
                        std::fs::remove_dir_all(restore_tmpdir2)
                    }).await??;
                }
                pg_backup.pg_verifybackup(&restore_tmpdir, restore_tmpdir.join("backup_manifest"), callback.clone()).await?;
            } else {
                bail!("Backup {} is not a full or incremental backup and cannot be restored.", record.id);
            }

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
        let backups_dir = self.get_backups_dir();

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
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if !metadata.is_dir() {
                    // Allow non-directories to continue to exist
                    to_remove.pop();
                    continue;
                }
                let Ok(uuid) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
                    continue;
                };

                let path = entry.path();
                let metadata_file = path.join("backup.json");
                let Ok(metadata) = BackupMetadata::from_file(&metadata_file) else {
                    continue;
                };
                to_remove.pop();

                let backup = metadata.into_backup(
                    BackupId::new(uuid.to_string()),
                    path.to_str().map(|s| s.to_string()),
                );
                backups.push(backup);
            }
            for path in to_remove {
                log::warn!("Removing incomplete or failed backup {}", path.display());
                _ = std::fs::remove_dir_all(&path);
            }
            backups.sort_by_key(|b| b.created_on);
            Ok(backups)
        })
        .map(map_join_error::<_, anyhow::Error>)
        .boxed()
    }
    fn get_backup(&self, backup_id: &BackupId) -> Operation<Backup> {
        let backups_dir = self.get_backups_dir();
        let backup_id = backup_id.clone();
        tokio::task::spawn_blocking(move || {
            let record = BackupRecord::from_file(backups_dir, backup_id.clone())?;
            Ok(record.metadata.into_backup(
                backup_id,
                record.metadata_dir.to_str().map(|s| s.to_string()),
            ))
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

    fn find_executable(&self, executable: &str) -> Result<PathBuf, anyhow::Error> {
        let path = self.portable_bin_path.join(executable);
        if !path.exists() {
            return Err(anyhow::anyhow!(
                "{} not found at {}: backup/restore not supported for this server version.",
                executable,
                path.display()
            ));
        }
        Ok(path)
    }

    pub async fn unpack_backup(
        &self,
        backup_data_dir: impl AsRef<Path>,
        target_dir: impl AsRef<Path>,
        callback: ProgressCallback,
    ) -> Result<(), anyhow::Error> {
        let backup_data_path = backup_data_dir.as_ref().to_path_buf();
        let data_file = backup_data_path.join("base.tar.gz");
        let wal_file = backup_data_path.join("pg_wal.tar.gz");
        let manifest_file = backup_data_path.join("backup_manifest");
        {
            let callback = callback.clone();
            let restore_tmpdir = target_dir.as_ref().to_path_buf();
            tokio::task::spawn_blocking(move || {
                // Note to reader: tar files may contain multiple
                // concatenated gzip streams, so we need to use a
                // MultiGzDecoder to unpack them.

                info!("Unpacking {data_file:?} to {restore_tmpdir:?}");
                callback.progress(None, "Unpacking data");
                let mut data_tar =
                    tar::Archive::new(flate2::read::MultiGzDecoder::new(File::open(data_file)?));
                data_tar.unpack(&restore_tmpdir)?;

                let wal_dir = restore_tmpdir.join("pg_wal");
                info!("Unpacking {wal_file:?} to {wal_dir:?}");
                callback.progress(None, "Unpacking WAL");
                let mut wal_tar =
                    tar::Archive::new(flate2::read::MultiGzDecoder::new(File::open(wal_file)?));
                wal_tar.unpack(wal_dir)?;

                std::fs::copy(manifest_file, restore_tmpdir.join("backup_manifest"))?;

                Ok::<_, std::io::Error>(())
            })
            .await??;
        }
        Ok(())
    }

    pub async fn pg_basebackup(
        &self,
        target_dir: impl AsRef<Path>,
        unix_path: impl AsRef<Path>,
        username: &str,
        callback: ProgressCallback,
    ) -> Result<(), anyhow::Error> {
        let pg_basebackup = self.find_executable("pg_basebackup")?;
        let mut cmd = Command::new(pg_basebackup);
        cmd.arg("--pgdata").arg(target_dir.as_ref());
        cmd.arg("--host").arg(unix_path.as_ref());
        cmd.arg("--username").arg(username);
        cmd.arg("--format=tar");
        cmd.arg("--checkpoint=fast");
        cmd.arg("--progress");
        cmd.arg("--compress=client-gzip");
        // Slows it down
        // cmd.arg("-r 1000");

        debug!("Running {cmd:?}");
        self.runner
            .run_lines(cmd, move |line| {
                callback.progress(None, &format!("running pg_basebackup: {}", line.trim()));
            })
            .await?;
        Ok(())
    }

    pub async fn pg_basebackup_incremental(
        &self,
        target_dir: impl AsRef<Path>,
        unix_path: impl AsRef<Path>,
        username: &str,
        incremental: impl AsRef<Path>,
        callback: ProgressCallback,
    ) -> Result<(), anyhow::Error> {
        let pg_basebackup = self.find_executable("pg_basebackup")?;
        let mut cmd = Command::new(pg_basebackup);
        cmd.arg("--pgdata").arg(target_dir.as_ref());
        cmd.arg("--host").arg(unix_path.as_ref());
        cmd.arg("--username").arg(username);
        cmd.arg("--format=tar");
        cmd.arg("--checkpoint=fast");
        cmd.arg("--progress");
        cmd.arg("--compress=client-gzip");
        cmd.arg("--incremental");
        cmd.arg(incremental.as_ref());
        // Slows it down
        // cmd.arg("-r 1000");

        debug!("Running {cmd:?}");
        self.runner
            .run_lines(cmd, move |line| {
                callback.progress(None, &format!("running pg_basebackup: {}", line.trim()));
            })
            .await?;
        Ok(())
    }

    pub async fn pg_combinebackup(
        &self,
        target_dir: impl AsRef<Path>,
        paths: &[impl AsRef<Path>],
        callback: ProgressCallback,
    ) -> Result<(), anyhow::Error> {
        let pg_combinebackup = self.find_executable("pg_combinebackup")?;
        let mut cmd = Command::new(pg_combinebackup);
        cmd.arg("--output").arg(target_dir.as_ref());
        for path in paths {
            cmd.arg(path.as_ref());
        }

        debug!("Running {cmd:?}");
        self.runner
            .run_lines(cmd, move |line| {
                callback.progress(None, &format!("running pg_combinebackup: {}", line.trim()));
            })
            .await?;
        Ok(())
    }

    pub async fn pg_verifybackup(
        &self,
        backup_dir: impl AsRef<Path>,
        backup_manifest: impl AsRef<Path>,
        callback: ProgressCallback,
    ) -> Result<(), anyhow::Error> {
        let pg_verifybackup = self.find_executable("pg_verifybackup")?;
        let mut cmd = Command::new(pg_verifybackup);
        cmd.arg("--progress");
        cmd.arg("--manifest-path").arg(backup_manifest.as_ref());
        cmd.arg(backup_dir.as_ref());

        debug!("Running {cmd:?}");
        self.runner
            .run_lines(cmd, move |line| {
                callback.progress(None, &format!("running pg_verifybackup: {}", line.trim()));
            })
            .await?;
        Ok(())
    }
}
