use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use futures_util::future;
use gel_cli_derive::IntoArgs;
use gel_cli_instance::instance::backup::{
    BackupStrategy, ProgressCallbackListener, RequestedBackupStrategy, RestoreType,
};
use gel_cli_instance::instance::{InstanceHandle, get_cloud_instance, get_local_instance};
use gel_tokio::InstanceName;

use crate::branding::BRANDING_CLI_CMD;
use crate::cloud;
use crate::locking::LockManager;
use crate::options::{CloudOptions, InstanceOptions};
use crate::portable::local::InstanceInfo;
use crate::print::msg;
use crate::question;

struct ProgressBar {
    bar: indicatif::ProgressBar,
}

impl Default for ProgressBar {
    fn default() -> Self {
        let bar =
            indicatif::ProgressBar::new_spinner().with_message(format!("{}", "Please wait..."));
        bar.enable_steady_tick(Duration::from_millis(100));
        Self { bar }
    }
}

impl ProgressCallbackListener for ProgressBar {
    fn progress(&self, progress: Option<f64>, message: &str) {
        if let Some(progress) = progress {
            self.bar.set_length(100);
            self.bar.set_position(progress as u64);
        }
        self.bar
            .set_message(format!("Current operation: {}...", message));
    }

    fn println(&self, msg: &str) {
        self.bar.println(msg);
    }
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct ListBackups {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    #[command(flatten)]
    pub instance_opts: InstanceOptions,

    /// Output in JSON format.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Backup {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    #[command(flatten)]
    pub instance_opts: InstanceOptions,

    /// Do not ask questions.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(clap::Args, IntoArgs, Clone, Debug)]
#[group(id = "backupspec", required = true)]
pub struct BackupSpec {
    #[arg(long)]
    pub backup_id: Option<String>,

    #[arg(long)]
    pub latest: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Restore {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    #[command(flatten)]
    pub instance_opts: InstanceOptions,

    #[command(flatten)]
    pub backup_spec: BackupSpec,

    /// Name of source instance to restore the backup from.
    #[arg(long)]
    #[arg(value_hint=clap::ValueHint::Other)] // TODO complete instance name
    pub source_instance: Option<InstanceName>,

    /// Do not ask questions.
    #[arg(long)]
    pub non_interactive: bool,
}

fn get_instance(
    opts: &crate::options::Options,
    instance_name: &InstanceName,
) -> anyhow::Result<InstanceHandle> {
    match instance_name {
        InstanceName::Local(name) => {
            let instance_info = InstanceInfo::try_read(name)?;
            let Some(instance_info) = instance_info else {
                return Err(anyhow::anyhow!("Remote instances not supported"));
            };
            let Some(install_info) = instance_info.installation else {
                return Err(anyhow::anyhow!("Instance {} not installed", name));
            };
            let bin_dir = install_info.base_path()?.join("bin");

            Ok(get_local_instance(
                name,
                bin_dir,
                install_info.version.specific().to_string(),
            )?)
        }
        InstanceName::Cloud(name) => {
            let client = cloud::client::CloudClient::new(&opts.cloud_options)?;
            client.ensure_authenticated()?;
            Ok(get_cloud_instance(name.clone(), client.api)?)
        }
    }
}

#[tokio::main]
pub async fn list(cmd: &ListBackups, opts: &crate::options::Options) -> anyhow::Result<()> {
    if cfg!(windows) {
        bail!("Instance backup/restore is not yet supported on Windows");
    }

    let inst_name = cmd.instance_opts.instance()?;
    let _lock = LockManager::lock_read_instance_async(&inst_name).await?;
    let instance = get_instance(opts, &inst_name)?.backup()?;
    let backups = instance.list_backups().await?;

    if cmd.json {
        println!("{}", serde_json::to_string_pretty(&backups)?);
    } else {
        use crate::table::{self, Cell, Row, Table};
        let mut table = Table::new();
        table.set_format(*table::FORMAT);
        table.set_titles(Row::new(
            ["ID", "Created", "Type", "Status", "Server Version"]
                .iter()
                .map(|x| table::header_cell(x))
                .collect(),
        ));
        for key in backups {
            let backup_type = match key.backup_strategy {
                BackupStrategy::Full => format!("{}", key.backup_type),
                other => format!("{} ({:?})", key.backup_type, other),
            };
            table.add_row(Row::new(vec![
                Cell::new(&key.id.to_string()),
                Cell::new(&humantime::format_rfc3339_seconds(key.created_on).to_string()),
                Cell::new(&backup_type),
                Cell::new(&key.status),
                Cell::new(&key.server_version),
            ]));
        }
        if !table.is_empty() {
            table.printstd();
        } else {
            println!("No backups found.")
        }
    }

    Ok(())
}

#[tokio::main]
pub async fn backup(cmd: &Backup, opts: &crate::options::Options) -> anyhow::Result<()> {
    if cfg!(windows) {
        bail!("Instance backup/restore is not yet supported on Windows");
    }

    let inst_name = cmd.instance_opts.instance()?;
    let _lock = LockManager::lock_read_instance_async(&inst_name).await?;
    let backup = get_instance(opts, &inst_name)?.backup()?;

    let prompt = format!(
        "Will create a backup for {inst_name:#}.\
        \n\nContinue?",
    );

    if !cmd.non_interactive && !question::Confirm::new(prompt).ask()? {
        return Ok(());
    }

    let progress_bar = ProgressBar::default();

    // If local, start a task to connect to the instance to keep it alive
    // This should live in InstanceBackup code, but we can't easily connect in there yet
    if let InstanceName::Local(_) = &inst_name {
        let cfg = gel_tokio::Builder::new()
            .instance(inst_name.clone())
            .with_fs()
            .build()?;
        progress_bar.progress(Some(0.0), "Waiting for instance to be ready");
        let ready = Arc::new(tokio::sync::Barrier::new(2));
        let ready2 = ready.clone();
        tokio::spawn(async move {
            use crate::branding::QUERY_TAG;
            use crate::connect::Connection;

            let mut conn = Connection::connect(&cfg, QUERY_TAG).await?;

            ready.wait().await;
            conn.ping_while(future::pending::<()>()).await;
            Ok::<_, anyhow::Error>(())
        });

        ready2.wait().await;
    }

    let backup = backup
        .backup(RequestedBackupStrategy::Auto, progress_bar.into())
        .await?;

    if let Some(backup_id) = backup {
        msg!("Successfully created a backup {backup_id} for {inst_name:#}");
    } else {
        msg!("Successfully created a backup for {inst_name:#}");
    }
    Ok(())
}

#[tokio::main]
pub async fn restore(cmd: &Restore, opts: &crate::options::Options) -> anyhow::Result<()> {
    if cfg!(windows) {
        bail!("Instance backup/restore is not yet supported on Windows");
    }

    let inst_name = cmd.instance_opts.instance()?;
    let _lock = LockManager::lock_instance_async(&inst_name).await?;
    let backup = get_instance(opts, &inst_name)?.backup()?;

    let stop_warning = if let InstanceName::Local(_) = &inst_name {
        "This will stop the instance and restore all branches from the backup. Any data not backed up will be lost. After the restore operation is completed, the instance will be restarted."
    } else {
        "This will restore all branches from the backup. Any data not backed up will be lost."
    };

    let prompt = format!(
        "Will restore {inst_name:#} from the specified backup. {stop_warning}\
        \n\nContinue?",
    );

    if !cmd.non_interactive && !question::Confirm::new(prompt).ask()? {
        return Ok(());
    }

    if let InstanceName::Local(inst_name) = &inst_name {
        let inst_name = inst_name.clone();
        tokio::task::spawn_blocking(move || super::control::do_stop(&inst_name)).await??;
    }

    let restore_type = if cmd.backup_spec.latest {
        RestoreType::Latest
    } else if let Some(backup_id) = cmd.backup_spec.backup_id.as_ref() {
        RestoreType::Specific(backup_id.clone())
    } else {
        unreachable!()
    };

    let progress_bar = ProgressBar::default();
    backup
        .restore(
            cmd.source_instance.clone(),
            restore_type,
            progress_bar.into(),
        )
        .await?;

    if let InstanceName::Local(inst_name) = &inst_name {
        let meta = InstanceInfo::read(inst_name)?;
        tokio::task::spawn_blocking(move || super::control::do_start(&meta)).await??;
    }

    msg!("{inst_name:#} has been restored successfully.");
    msg!("To connect to the instance run:");
    msg!("  {BRANDING_CLI_CMD} -I {inst_name}");
    Ok(())
}
