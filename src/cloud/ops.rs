use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use anyhow::Context;
use const_format::concatcp;
use gel_tokio::credentials::Credentials;
use gel_tokio::Builder;
use indicatif::ProgressBar;
use tokio::time::{sleep, timeout};

use crate::branding::BRANDING_CLOUD;
use crate::cloud::client::{CloudClient, ErrorResponse};
use crate::collect::Collector;
use crate::options::CloudOptions;
use crate::portable::instance::status::{RemoteStatus, RemoteType};
use crate::question;

const OPERATION_WAIT_TIME: Duration = Duration::from_secs(20 * 60);
const POLLING_INTERVAL: Duration = Duration::from_secs(2);
const SPINNER_TICK: Duration = Duration::from_millis(100);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CloudInstance {
    pub id: String,
    name: String,
    org_slug: String,
    dsn: String,
    pub status: String,
    pub version: String,
    pub region: String,
    pub tier: CloudTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls_ca: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_url: Option<String>,
    pub billables: Vec<CloudInstanceResource>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CloudInstanceResource {
    pub name: String,
    pub display_name: String,
    pub display_unit: String,
    pub display_quota: String,
}

impl CloudInstance {
    pub async fn as_credentials(&self, secret_key: &str) -> anyhow::Result<Credentials> {
        let config = Builder::new()
            .secret_key(secret_key)
            .instance(&format!("{}/{}", self.org_slug, self.name))?
            .build_env()
            .await?;
        let mut creds = config.as_credentials()?;
        // TODO(tailhook) can this be emitted from as_credentials()?
        creds.tls_ca.clone_from(&self.tls_ca);
        Ok(creds)
    }
}

impl RemoteStatus {
    async fn from_cloud_instance(
        cloud_client: &CloudClient,
        cloud_instance: &CloudInstance,
    ) -> anyhow::Result<Self> {
        let secret_key = cloud_client.secret_key.clone().unwrap();
        let credentials = cloud_instance.as_credentials(&secret_key).await?;
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

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
pub struct Org {
    pub id: String,
    pub name: String,
    pub preferred_payment_method: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
pub struct Region {
    pub name: String,
    pub platform: String,
    pub platform_region: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct Version {
    pub version: String,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
pub struct Price {
    pub billable: String,
    pub unit_price_cents: String,
    pub units_bundled: Option<String>,
    pub units_default: Option<String>,
}

pub type Prices = HashMap<CloudTier, HashMap<String, Vec<Price>>>;

#[derive(Debug, serde::Deserialize)]
struct Billable {
    id: String,
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct PricesResponse {
    prices: Prices,
    billables: Vec<Billable>,
}

#[derive(Debug, serde::Serialize)]
pub struct CloudInstanceResourceRequest {
    pub name: String,
    pub value: String,
}

#[derive(
    Debug, serde::Serialize, serde::Deserialize, Hash, PartialEq, Eq, Clone, Copy, clap::ValueEnum,
)]
pub enum CloudTier {
    Pro,
    Free,
}

impl fmt::Display for CloudTier {
    fn fmt(&self, f: &mut fmt::Formatter) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, serde::Serialize)]
pub struct CloudInstanceCreate {
    pub name: String,
    pub org: String,
    pub version: String,
    pub region: Option<String>,
    pub requested_resources: Option<Vec<CloudInstanceResourceRequest>>,
    pub tier: Option<CloudTier>,
    pub source_instance_id: Option<String>,
    pub source_backup_id: Option<String>,
    // #[serde(skip_serializing_if = "Option::is_none")]
    // pub default_database: Option<String>,
    // #[serde(skip_serializing_if = "Option::is_none")]
    // pub default_user: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CloudInstanceResize {
    pub name: String,
    pub org: String,
    pub requested_resources: Option<Vec<CloudInstanceResourceRequest>>,
    pub tier: Option<CloudTier>,
}

#[derive(Debug, serde::Serialize)]
pub struct CloudInstanceUpgrade {
    pub name: String,
    pub org: String,
    pub version: String,
    pub force: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct CloudInstanceRestart {}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    InProgress,
    Failed,
    Completed,
}

#[derive(Debug, serde::Deserialize)]
pub struct CloudOperation {
    pub id: String,
    pub status: OperationStatus,
    pub description: String,
    pub message: String,
    pub subsequent_id: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_current_region(client: &CloudClient) -> anyhow::Result<Region> {
    let url = "region/self";
    client.get(url).await
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_versions(client: &CloudClient) -> anyhow::Result<Vec<Version>> {
    let url = "versions";
    client.get(url).await
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_prices(client: &CloudClient) -> anyhow::Result<Prices> {
    let url = "pricing";
    let mut resp: PricesResponse = client.get(url).await?;

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
    inst: &str,
    org: &str,
    client: &CloudClient,
) -> anyhow::Result<Option<CloudInstance>> {
    client
        .get(format!("orgs/{org}/instances/{inst}"))
        .await
        .map(Some)
        .or_else(|e| match e.downcast_ref::<ErrorResponse>() {
            Some(ErrorResponse {
                code: reqwest::StatusCode::NOT_FOUND,
                ..
            }) => Ok(None),
            _ => Err(e),
        })
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_org(org: &str, client: &CloudClient) -> anyhow::Result<Org> {
    client.get(format!("orgs/{org}")).await
}

pub(crate) async fn wait_for_operation(
    mut operation: CloudOperation,
    client: &CloudClient,
) -> anyhow::Result<()> {
    let spinner =
        ProgressBar::new_spinner().with_message(format!("Monitoring {}...", operation.description));
    spinner.enable_steady_tick(SPINNER_TICK);

    let mut url = format!("operations/{}", operation.id);
    let deadline = Instant::now() + OPERATION_WAIT_TIME;

    let mut original_error = None;

    while Instant::now() < deadline {
        match (operation.status, operation.subsequent_id) {
            (OperationStatus::Failed, Some(subsequent_id)) => {
                original_error = original_error.or(Some(operation.message));

                url = format!("operations/{subsequent_id}");
                operation = client.get(&url).await?;
            }
            (OperationStatus::Failed, None) => {
                anyhow::bail!(original_error.unwrap_or(operation.message));
            }
            (OperationStatus::InProgress, _) => {
                sleep(POLLING_INTERVAL).await;
                operation = client.get(&url).await?;
            }
            (OperationStatus::Completed, _) => {
                if let Some(message) = original_error {
                    anyhow::bail!(message)
                } else {
                    return Ok(());
                }
            }
        }
    }

    anyhow::bail!("Operation is taking too long, stopping monitor.")
}

#[tokio::main(flavor = "current_thread")]
pub async fn create_cloud_instance(
    client: &CloudClient,
    request: &CloudInstanceCreate,
) -> anyhow::Result<()> {
    let url = format!("orgs/{}/instances", request.org);
    let operation: CloudOperation = client.post(url, request).await.or_else(|e| match e
        .downcast_ref::<ErrorResponse>(
    ) {
        Some(ErrorResponse {
            code: reqwest::StatusCode::NOT_FOUND,
            ..
        }) => {
            anyhow::bail!("Organization \"{}\" does not exist.", request.org);
        }
        _ => Err(e),
    })?;
    wait_for_operation(operation, client).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn resize_cloud_instance(
    client: &CloudClient,
    request: &CloudInstanceResize,
) -> anyhow::Result<()> {
    let url = format!("orgs/{}/instances/{}", request.org, request.name);
    let operation: CloudOperation = client.put(url, request).await.or_else(|e| match e
        .downcast_ref::<ErrorResponse>()
    {
        Some(ErrorResponse {
            code: reqwest::StatusCode::NOT_FOUND,
            ..
        }) => {
            anyhow::bail!(
                "Instance \"{}/{}\" does not exist.",
                request.org,
                request.name
            );
        }
        _ => Err(e),
    })?;
    wait_for_operation(operation, client).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn upgrade_cloud_instance(
    client: &CloudClient,
    request: &CloudInstanceUpgrade,
) -> anyhow::Result<()> {
    let url = format!("orgs/{}/instances/{}", request.org, request.name);
    let operation: CloudOperation = client.put(url, request).await?;
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
    name: &str,
    org: &str,
    options: &CloudOptions,
) -> anyhow::Result<()> {
    let client = CloudClient::new(options)?;
    client.ensure_authenticated()?;
    let operation: CloudOperation = client
        .post(
            format!("orgs/{org}/instances/{name}/restart"),
            &CloudInstanceRestart {},
        )
        .await?;
    wait_for_operation(operation, &client).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn destroy_cloud_instance(
    name: &str,
    org: &str,
    options: &CloudOptions,
) -> anyhow::Result<()> {
    let client = CloudClient::new(options)?;
    client.ensure_authenticated()?;
    let operation: CloudOperation = client
        .delete(format!("orgs/{org}/instances/{name}"))
        .await?;
    wait_for_operation(operation, &client).await?;
    Ok(())
}

async fn get_instances(client: &CloudClient) -> anyhow::Result<Vec<CloudInstance>> {
    timeout(Duration::from_secs(30), client.get("instances/"))
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
                errors.add(e.context(format!(
                    "probing {}/{}",
                    cloud_instance.org_slug, cloud_instance.name
                )));
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
