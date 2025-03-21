use color_print::cformat;
use gel_cli_derive::IntoArgs;

use crate::branding::{BRANDING_CLI_CMD, BRANDING_CLOUD};
use crate::cloud;
use crate::options::CloudOptions;
use crate::portable::options::InstanceName;
use crate::print::msg;
use crate::question;

pub fn list(cmd: &ListBackups, opts: &crate::options::Options) -> anyhow::Result<()> {
    match &cmd.instance {
        InstanceName::Local(_) => Err(opts.error(
            clap::error::ErrorKind::InvalidValue,
            cformat!("list-backups can only operate on {BRANDING_CLOUD} instances."),
        ))?,
        InstanceName::Cloud {
            org_slug: org,
            name,
        } => list_cloud_backups_cmd(cmd, org, name, opts),
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

fn list_cloud_backups_cmd(
    cmd: &ListBackups,
    org_slug: &str,
    name: &str,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    let client = cloud::client::CloudClient::new(&opts.cloud_options)?;
    client.ensure_authenticated()?;

    cloud::backups::list_cloud_instance_backups(&client, org_slug, name, cmd.json)?;

    Ok(())
}

pub fn backup(cmd: &Backup, opts: &crate::options::Options) -> anyhow::Result<()> {
    match &cmd.instance {
        InstanceName::Local(_) => Err(opts.error(
            clap::error::ErrorKind::InvalidValue,
            cformat!("Only {BRANDING_CLOUD} instances can be backed up using this command."),
        ))?,
        InstanceName::Cloud {
            org_slug: org,
            name,
        } => backup_cloud_cmd(cmd, org, name, opts),
    }
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

fn backup_cloud_cmd(
    cmd: &Backup,
    org_slug: &str,
    name: &str,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    let client = cloud::client::CloudClient::new(&opts.cloud_options)?;
    client.ensure_authenticated()?;

    let inst_name = InstanceName::Cloud {
        org_slug: org_slug.to_string(),
        name: name.to_string(),
    };

    let prompt = format!(
        "Will create a backup for the {BRANDING_CLOUD} instance \"{inst_name}\":\
        \n\nContinue?",
    );

    if !cmd.non_interactive && !question::Confirm::new(prompt).ask()? {
        return Ok(());
    }

    cloud::backups::backup_cloud_instance(&client, org_slug, name)?;

    msg!("Successfully created a backup for {BRANDING_CLOUD} instance {inst_name}");
    Ok(())
}

pub fn restore(cmd: &Restore, opts: &crate::options::Options) -> anyhow::Result<()> {
    match &cmd.instance {
        InstanceName::Local(_) => Err(opts.error(
            clap::error::ErrorKind::InvalidValue,
            cformat!("Only {BRANDING_CLOUD} instances can be restored."),
        ))?,
        InstanceName::Cloud {
            org_slug: org,
            name,
        } => restore_cloud_cmd(cmd, org, name, opts),
    }
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

fn restore_cloud_cmd(
    cmd: &Restore,
    org_slug: &str,
    name: &str,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    let backup = &cmd.backup_spec;

    let client = cloud::client::CloudClient::new(&opts.cloud_options)?;
    client.ensure_authenticated()?;

    let inst_name = InstanceName::Cloud {
        org_slug: org_slug.to_string(),
        name: name.to_string(),
    };

    let source_inst = match &cmd.source_instance {
        Some(InstanceName::Local(_)) => Err(opts.error(
            clap::error::ErrorKind::InvalidValue,
            cformat!("--source-instance can only be a valid {BRANDING_CLOUD} instance"),
        ))?,
        Some(InstanceName::Cloud { org_slug, name }) => {
            let inst = cloud::ops::find_cloud_instance_by_name(name, org_slug, &client)?
                .ok_or_else(|| anyhow::anyhow!("instance not found"))?;
            Some(inst)
        }
        None => None,
    };

    let prompt = format!(
        "Will restore the {BRANDING_CLOUD} instance \"{inst_name}\" from the specified backup:\
        \n\nContinue?",
    );

    if !cmd.non_interactive && !question::Confirm::new(prompt).ask()? {
        return Ok(());
    }

    cloud::backups::restore_cloud_instance(
        &client,
        org_slug,
        name,
        backup.latest,
        backup.backup_id.clone(),
        source_inst.map(|i| i.id),
    )?;

    msg!("{BRANDING_CLOUD} instance {inst_name} has been restored successfully.");
    msg!("To connect to the instance run:");
    msg!("  {BRANDING_CLI_CMD} -I {inst_name}");
    Ok(())
}
