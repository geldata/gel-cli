use std::collections::HashMap;

#[derive(Debug, serde::Deserialize)]
pub struct User {
    pub name: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct UserSessionCreated {
    pub id: String,
    pub auth_url: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct UserSession {
    pub id: String,
    pub token: Option<String>,
    pub auth_url: String,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Backup {
    pub id: String,

    #[serde(with = "humantime_serde")]
    pub created_on: std::time::SystemTime,

    pub status: String,
    pub r#type: String,
    pub edgedb_version: String,
}

#[derive(Debug, serde::Serialize)]
pub struct CloudInstanceBackup {
    pub name: String,
    pub org: String,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct CloudInstanceRestore {
    pub backup_id: Option<String>,
    pub latest: bool,
    pub source_instance_id: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CloudInstance {
    pub id: String,
    pub name: String,
    pub org_slug: String,
    pub dsn: String,
    pub status: String,
    pub version: String,
    pub region: String,
    pub tier: CloudTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_ca: Option<String>,
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
pub struct Billable {
    pub id: String,
    pub name: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct PricesResponse {
    pub prices: Prices,
    pub billables: Vec<Billable>,
}

#[derive(Debug, serde::Serialize)]
pub struct CloudInstanceResourceRequest {
    pub name: String,
    pub value: String,
}

#[derive(
    Debug,
    serde::Serialize,
    serde::Deserialize,
    Hash,
    PartialEq,
    Eq,
    Clone,
    Copy,
    clap::ValueEnum,
    derive_more::Display,
)]
pub enum CloudTier {
    Pro,
    Free,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct SecretKey {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub scopes: Vec<String>,

    #[serde(with = "humantime_serde")]
    pub created_on: std::time::SystemTime,

    #[serde(with = "humantime_serde")]
    pub expires_on: Option<std::time::SystemTime>,

    pub secret_key: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CreateSecretKeyInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub ttl: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CloudInstanceCreate {
    pub name: String,
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
    pub requested_resources: Option<Vec<CloudInstanceResourceRequest>>,
    pub tier: Option<CloudTier>,
}

#[derive(Debug, serde::Serialize)]
pub struct CloudInstanceUpgrade {
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

pub struct Log {
    pub logs: Vec<LogEntry>,
}

#[derive(Debug, serde::Deserialize)]
pub struct LogEntry {
    pub service: String,
    pub severity: String,
    #[serde(with = "humantime_serde")]
    pub time: std::time::SystemTime,
    pub log: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct Logs {
    pub logs: Vec<LogEntry>,
}
