use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Context;
use const_format::concatcp;
use gel_cli_instance::cloud::{
    CloudInstance, CloudInstanceCreate, CloudInstanceResize, CloudInstanceUpgrade, CloudOperation,
    OperationStatus, Org, Prices, Region, Version,
};
use gel_dsn::gel::CredentialsFile;
use gel_tokio::{Builder, CloudName, InstanceName};
use indicatif::ProgressBar;
use tokio::time::{sleep, timeout};

use crate::branding::{BRANDING, BRANDING_CLOUD};
use crate::cloud::client::CloudClient;
use crate::collect::Collector;
use crate::options::CloudOptions;
use crate::portable::instance::status::{RemoteStatus, RemoteType};
use crate::question;

const OPERATION_WAIT_TIME: Duration = Duration::from_secs(20 * 60);
const POLLING_INTERVAL: Duration = Duration::from_secs(2);
const SPINNER_TICK: Duration = Duration::from_millis(100);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CloudInstanceResource {
    pub name: String,
    pub display_name: String,
    pub display_unit: String,
    pub display_quota: String,
}

pub async fn as_credentials(
    cloud_instance: &CloudInstance,
    secret_key: &str,
) -> anyhow::Result<CredentialsFile> {
    let config = Builder::new()
        .secret_key(secret_key)
        .instance(InstanceName::from(CloudName {
            org_slug: cloud_instance.org_slug.clone(),
            name: cloud_instance.name.clone(),
        }))
        .build()?;
    let mut creds = config.as_credentials()?;
    // TODO(tailhook) can this be emitted from as_credentials()?
    creds.tls_ca.clone_from(&cloud_instance.tls_ca);
    Ok(creds)
}

impl RemoteStatus {
    async fn from_cloud_instance(
        cloud_client: &CloudClient,
        cloud_instance: &CloudInstance,
    ) -> anyhow::Result<Self> {
        let secret_key = cloud_client.secret_key.clone().unwrap();
        let credentials = as_credentials(cloud_instance, &secret_key).await?;
        Ok(Self {
            name: format!("{}/{}", cloud_instance.org_slug, cloud_instance.name),
            type_: RemoteType::Cloud {
                instance_id: cloud_instance.id.clone(),
            },
            credentials,
            version: Some(cloud_instance.version.clone()),
            connection: None,
            instance_status: Some(cloud_instance.status.clone()),
            location: format!("\u{2601} {}", cloud_instance.region),
        })
    }
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_current_region(client: &CloudClient) -> anyhow::Result<Region> {
    Ok(client.api.get_current_region().await?)
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_versions(client: &CloudClient) -> anyhow::Result<Vec<Version>> {
    Ok(client.api.get_versions().await?)
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_prices(client: &CloudClient) -> anyhow::Result<Prices> {
    let mut resp = client.api.get_prices().await?;

    let billable_id_to_name: HashMap<String, String> = resp
        .billables
        .iter()
        .map(|billable| (billable.id.clone(), billable.name.clone()))
        .collect();

    for tier_prices in resp.prices.values_mut() {
        for region_prices in tier_prices.values_mut() {
            for price in region_prices {
                price.billable = billable_id_to_name
                    .get(&price.billable)
                    .context(format!("could not map billable {} to name", price.billable))?
                    .to_string();
            }
        }
    }

    Ok(resp.prices)
}

#[tokio::main(flavor = "current_thread")]
pub async fn find_cloud_instance_by_name(
    instance: &CloudName,
    client: &CloudClient,
) -> anyhow::Result<Option<CloudInstance>> {
    Ok(Some(client.api.get_instance(instance).await?))
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_org(org: &str, client: &CloudClient) -> anyhow::Result<Org> {
    Ok(client.api.get_org(org).await?)
}

pub(crate) async fn wait_for_operation(
    mut operation: CloudOperation,
    client: &CloudClient,
) -> anyhow::Result<()> {
    // NOTE: This is a temporary fix to replace "EdgeDB" until the cloud updates the message.
    let spinner = ProgressBar::new_spinner().with_message(format!(
        "Monitoring {}...",
        operation.description.replace("EdgeDB", BRANDING)
    ));
    spinner.enable_steady_tick(SPINNER_TICK);

    let deadline = Instant::now() + OPERATION_WAIT_TIME;

    let mut original_error = None;
    let mut id = operation.id;
    while Instant::now() < deadline {
        match (operation.status, operation.subsequent_id) {
            (OperationStatus::Failed, Some(subsequent_id)) => {
                original_error =
                    original_error.or(Some(operation.message.replace("EdgeDB", BRANDING)));
                id = subsequent_id;
            }
            (OperationStatus::Failed, None) => {
                anyhow::bail!(
                    original_error.unwrap_or(operation.message.replace("EdgeDB", BRANDING))
                );
            }
            (OperationStatus::InProgress, _) => {
                sleep(POLLING_INTERVAL).await;
            }
            (OperationStatus::Completed, _) => {
                if let Some(message) = original_error {
                    anyhow::bail!(message)
                } else {
                    return Ok(());
                }
            }
        }

        operation = client.api.get_operation(&id).await?;
    }

    anyhow::bail!("Operation is taking too long, stopping monitor.")
}

#[tokio::main(flavor = "current_thread")]
pub async fn create_cloud_instance(
    client: &CloudClient,
    org_slug: &str,
    request: CloudInstanceCreate,
) -> anyhow::Result<()> {
    let operation: CloudOperation = client.api.create_instance(org_slug, request).await?;
    wait_for_operation(operation, client).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn resize_cloud_instance(
    client: &CloudClient,
    name: &CloudName,
    request: CloudInstanceResize,
) -> anyhow::Result<()> {
    let operation: CloudOperation = client.api.resize_instance(name, request).await?;
    wait_for_operation(operation, client).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn upgrade_cloud_instance(
    client: &CloudClient,
    name: &CloudName,
    request: CloudInstanceUpgrade,
) -> anyhow::Result<()> {
    let operation: CloudOperation = client.api.upgrade_instance(name, request).await?;
    wait_for_operation(operation, client).await?;
    Ok(())
}

pub fn prompt_cloud_login(client: &mut CloudClient) -> anyhow::Result<()> {
    let mut q = question::Confirm::new(concatcp!(
        "Not authenticated to ",
        BRANDING_CLOUD,
        " yet, log in now?"
    ));
    if q.default(true).ask()? {
        crate::cloud::auth::do_login(client)?;
        client.reinit()?;
        client.ensure_authenticated()?;
        Ok(())
    } else {
        anyhow::bail!("Aborted.");
    }
}

#[tokio::main(flavor = "current_thread")]
pub async fn restart_cloud_instance(
    name: &CloudName,
    options: &CloudOptions,
) -> anyhow::Result<()> {
    let client = CloudClient::new(options)?;
    client.ensure_authenticated()?;
    let operation = client.api.restart_instance(name).await?;
    wait_for_operation(operation, &client).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn destroy_cloud_instance(
    name: &CloudName,
    options: &CloudOptions,
) -> anyhow::Result<()> {
    let client = CloudClient::new(options)?;
    client.ensure_authenticated()?;
    let operation: CloudOperation = client.api.delete_instance(name).await?;
    wait_for_operation(operation, &client).await?;
    Ok(())
}

async fn get_instances(client: &CloudClient) -> anyhow::Result<Vec<CloudInstance>> {
    timeout(Duration::from_secs(30), client.api.list_instances())
        .await
        .or_else(|_| anyhow::bail!("{BRANDING_CLOUD} instances API timed out"))?
        .context(concatcp!(
            "failed to list instances in ",
            BRANDING_CLOUD,
            " API"
        ))
}

pub async fn list(
    client: CloudClient,
    errors: &Collector<anyhow::Error>,
) -> anyhow::Result<Vec<RemoteStatus>> {
    client.ensure_authenticated()?;
    let cloud_instances = get_instances(&client).await?;
    let mut rv = Vec::new();
    for cloud_instance in cloud_instances {
        match RemoteStatus::from_cloud_instance(&client, &cloud_instance).await {
            Ok(status) => rv.push(status),
            Err(e) => {
                errors.add(e.context(format!("probing {}", cloud_instance.name)));
            }
        }
    }
    Ok(rv)
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_status(
    client: &CloudClient,
    instance: &CloudInstance,
) -> anyhow::Result<RemoteStatus> {
    client.ensure_authenticated()?;
    RemoteStatus::from_cloud_instance(client, instance).await
}

#[tokio::main(flavor = "current_thread")]
pub async fn logs_cloud_instance(
    name: &CloudName,
    limit: Option<usize>,
    options: &CloudOptions,
) -> anyhow::Result<()> {
    let client = CloudClient::new(options)?;
    client.ensure_authenticated()?;
    let logs = client
        .api
        .get_instance_logs(name, limit, None, None, None)
        .await?;
    for log in logs.logs {
        println!(
            "[{} {} {}] {}",
            humantime::format_rfc3339_seconds(log.time),
            log.severity,
            log.service,
            log.log
        );
    }
    Ok(())
}
