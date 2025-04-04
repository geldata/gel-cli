use anyhow::Context;
use color_print::cformat;
use gel_cli_derive::IntoArgs;
use gel_cli_instance::cloud::{CloudInstanceResize, CloudInstanceResourceRequest, CloudTier};
use gel_tokio::{CloudName, InstanceName};

use crate::branding::{BRANDING_CLI_CMD, BRANDING_CLOUD};
use crate::cloud;
use crate::options::CloudOptions;
use crate::portable::options::CloudInstanceBillables;
use crate::print::msg;
use crate::question;

pub fn run(cmd: &Command, opts: &crate::options::Options) -> anyhow::Result<()> {
    match &cmd.instance {
        InstanceName::Local(_) => Err(opts.error(
            clap::error::ErrorKind::InvalidValue,
            cformat!("Only {BRANDING_CLOUD} instances can be resized."),
        ))?,
        InstanceName::Cloud(name) => resize_cloud_cmd(cmd, name, opts),
    }
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Instance to resize.
    #[arg(short = 'I', long, required = true)]
    #[arg(value_hint=clap::ValueHint::Other)] // TODO complete instance name
    pub instance: InstanceName,

    #[command(flatten)]
    pub billables: CloudInstanceBillables,

    /// Do not ask questions.
    #[arg(long)]
    pub non_interactive: bool,
}

fn resize_cloud_cmd(
    cmd: &Command,
    name: &CloudName,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    let billables = &cmd.billables;

    if billables.tier.is_none()
        && billables.compute_size.is_none()
        && billables.storage_size.is_none()
    {
        Err(opts.error(
            clap::error::ErrorKind::MissingRequiredArgument,
            cformat!(
                "Either <bold>--tier</bold>, <bold>--compute-size</bold>, \
            or <bold>--storage-size</bold> must be specified."
            ),
        ))?;
    }

    let client = cloud::client::CloudClient::new(&opts.cloud_options)?;
    client.ensure_authenticated()?;

    let inst_name = InstanceName::Cloud(name.clone());

    let inst = cloud::ops::find_cloud_instance_by_name(name, &client)?
        .ok_or_else(|| anyhow::anyhow!("instance not found"))?;

    let mut compute_size = billables.compute_size.clone();
    let mut storage_size = billables.storage_size.clone();
    let mut resources_display_vec: Vec<String> = vec![];

    if let Some(tier) = billables.tier {
        if tier == inst.tier && compute_size.is_none() && storage_size.is_none() {
            Err(opts.error(
                clap::error::ErrorKind::InvalidValue,
                cformat!(
                    "Instance \"{inst_name}\" is already a {tier:?} \
                instance."
                ),
            ))?;
        }

        if tier == CloudTier::Free {
            if compute_size.is_some() {
                Err(opts.error(
                    clap::error::ErrorKind::ArgumentConflict,
                    cformat!(
                        "The <bold>--compute-size</bold> option can \
                    only be specified for Pro instances."
                    ),
                ))?;
            }
            if storage_size.is_some() {
                Err(opts.error(
                    clap::error::ErrorKind::ArgumentConflict,
                    cformat!(
                        "The <bold>--storage-size</bold> option can \
                    only be specified for Pro instances."
                    ),
                ))?;
            }
        }

        if tier != inst.tier {
            resources_display_vec.push(format!("New Tier: {tier:?}",));

            if storage_size.is_none() || compute_size.is_none() {
                let prices = cloud::ops::get_prices(&client)?;
                let tier_prices = prices.get(&tier).context(format!(
                    "could not download pricing information for the {tier} tier"
                ))?;
                let region_prices = tier_prices.get(&inst.region).context(format!(
                    "could not download pricing information for the {} region",
                    inst.region
                ))?;
                if compute_size.is_none() {
                    compute_size = Some(
                        region_prices
                            .iter()
                            .find(|&price| price.billable == "compute")
                            .context("could not download pricing information for compute")?
                            .units_default
                            .clone()
                            .context("could not find default value for compute")?,
                    );
                }
                if storage_size.is_none() {
                    storage_size = Some(
                        region_prices
                            .iter()
                            .find(|&price| price.billable == "storage")
                            .context("could not download pricing information for storage")?
                            .units_default
                            .clone()
                            .context("could not find default value for storage")?,
                    );
                }
            }
        }
    }

    let mut req_resources = vec![];

    if let Some(compute_size) = compute_size {
        req_resources.push(CloudInstanceResourceRequest {
            name: "compute".to_string(),
            value: compute_size.clone(),
        });
        resources_display_vec.push(format!(
            "New Compute Size: {} compute unit{}",
            compute_size,
            if compute_size == "1" { "" } else { "s" },
        ));
    }

    if let Some(storage_size) = storage_size {
        req_resources.push(CloudInstanceResourceRequest {
            name: "storage".to_string(),
            value: storage_size.clone(),
        });
        resources_display_vec.push(format!(
            "New Storage Size: {} gigabyte{}",
            storage_size,
            if storage_size == "1" { "" } else { "s" },
        ));
    }

    let mut resources_display = resources_display_vec.join("\n");
    if !resources_display.is_empty() {
        resources_display = format!("\n{resources_display}");
    }

    let prompt = format!(
        "Will resize the {BRANDING_CLOUD} instance \"{inst_name}\" as follows:\
        \n\
        {resources_display}\
        \n\nContinue?",
    );

    if !cmd.non_interactive && !question::Confirm::new(prompt).ask()? {
        return Ok(());
    }

    for res in req_resources {
        let request = CloudInstanceResize {
            requested_resources: Some(vec![res]),
            tier: billables.tier,
        };
        cloud::ops::resize_cloud_instance(&client, name, request)?;
    }
    msg!("{BRANDING_CLOUD} instance {inst_name} has been resized successfuly.");
    msg!("To connect to the instance run:");
    msg!("  {BRANDING_CLI_CMD} -I {inst_name}");
    Ok(())
}
