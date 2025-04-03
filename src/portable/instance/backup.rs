use std::time::Duration;

use gel_cli_derive::IntoArgs;
use gel_cli_instance::instance::backup::{ProgressCallbackListener, RestoreType};
use gel_cli_instance::instance::{InstanceHandle, get_cloud_instance, get_local_instance};

use crate::branding::{BRANDING_CLI_CMD, BRANDING_CLOUD};
use crate::cloud;
use crate::options::CloudOptions;
use crate::portable::options::InstanceName;
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
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct ListBackups {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Instance to list backups for.
    #[arg(short = 'I', long, required = true)]
    #[arg(value_hint=clap::ValueHint::Other)] // TODO complete instance name
    pub instance: InstanceName,

    /// Output in JSON format.
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Backup {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Instance to restore.
    #[arg(short = 'I', long, required = true)]
    #[arg(value_hint=clap::ValueHint::Other)] // TODO complete instance name
    pub instance: InstanceName,

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

    /// Instance to restore.
    #[arg(short = 'I', long, required = true)]
    #[arg(value_hint=clap::ValueHint::Other)] // TODO complete instance name
    pub instance: InstanceName,

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
    match instance_name.clone().into() {
        gel_dsn::gel::InstanceName::Local(name) => Ok(get_local_instance(&name)?),
        gel_dsn::gel::InstanceName::Cloud(name) => {
            let client = cloud::client::CloudClient::new(&opts.cloud_options)?;
            client.ensure_authenticated()?;
            Ok(get_cloud_instance(name, client.api)?)
        }
    }
}

#[tokio::main]
pub async fn list(cmd: &ListBackups, opts: &crate::options::Options) -> anyhow::Result<()> {
    let instance = get_instance(opts, &cmd.instance)?.backup()?;
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
            table.add_row(Row::new(vec![
                Cell::new(&key.id.to_string()),
                Cell::new(&humantime::format_rfc3339_seconds(key.created_on).to_string()),
                Cell::new(&key.backup_type.to_string()),
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
    let inst_name = cmd.instance.clone();
    let backup = get_instance(opts, &cmd.instance)?.backup()?;

    let prompt = format!(
        "Will create a backup for the {BRANDING_CLOUD} instance \"{inst_name}\".\
        \n\nContinue?",
    );

    if !cmd.non_interactive && !question::Confirm::new(prompt).ask()? {
        return Ok(());
    }

    let progress_bar = ProgressBar::default();
    let backup = backup.backup(progress_bar.into()).await?;

    if let Some(backup_id) = backup {
        msg!("Successfully created a backup {backup_id} for {BRANDING_CLOUD} instance {inst_name}");
    } else {
        msg!("Successfully created a backup for {BRANDING_CLOUD} instance {inst_name}");
    }
    Ok(())
}

#[tokio::main]
pub async fn restore(cmd: &Restore, opts: &crate::options::Options) -> anyhow::Result<()> {
    let inst_name = cmd.instance.clone();
    let backup = get_instance(opts, &cmd.instance)?.backup()?;

    let prompt = format!(
        "Will restore the {BRANDING_CLOUD} instance \"{inst_name}\" from the specified backup.\
        \n\nContinue?",
    );

    if !cmd.non_interactive && !question::Confirm::new(prompt).ask()? {
        return Ok(());
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
            cmd.source_instance.clone().map(|x| x.into()),
            restore_type,
            progress_bar.into(),
        )
        .await?;

    msg!("{BRANDING_CLOUD} instance {inst_name} has been restored successfully.");
    msg!("To connect to the instance run:");
    msg!("  {BRANDING_CLI_CMD} -I {inst_name}");
    Ok(())
}
