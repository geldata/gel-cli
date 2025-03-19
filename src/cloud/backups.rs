use crate::table::{self, Cell, Row, Table};

use crate::cloud::client::{CloudClient, ErrorResponse};
use crate::cloud::ops::{CloudOperation, wait_for_operation};

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

#[derive(Debug, serde::Serialize)]
pub struct CloudInstanceRestore {
    pub name: String,
    pub org: String,
    pub backup_id: Option<String>,
    pub latest: bool,
    pub source_instance_id: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
pub async fn backup_cloud_instance(
    client: &CloudClient,
    request: &CloudInstanceBackup,
) -> anyhow::Result<()> {
    let url = format!("orgs/{}/instances/{}/backups", request.org, request.name);
    let operation: CloudOperation = client.post(url, request).await.or_else(|e| match e
        .downcast_ref::<ErrorResponse>(
    ) {
        Some(ErrorResponse {
            code: reqwest::StatusCode::NOT_FOUND,
            ..
        }) => {
            anyhow::bail!("specified instance could not be found",);
        }
        _ => Err(e),
    })?;
    wait_for_operation(operation, client).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn restore_cloud_instance(
    client: &CloudClient,
    request: &CloudInstanceRestore,
) -> anyhow::Result<()> {
    let url = format!("orgs/{}/instances/{}/restore", request.org, request.name);
    let operation: CloudOperation = client.post(url, request).await.or_else(|e| match e
        .downcast_ref::<ErrorResponse>(
    ) {
        Some(ErrorResponse {
            code: reqwest::StatusCode::NOT_FOUND,
            ..
        }) => {
            anyhow::bail!("specified instance or backup could not be found",);
        }
        _ => Err(e),
    })?;
    wait_for_operation(operation, client).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn list_cloud_instance_backups(
    client: &CloudClient,
    org_slug: &str,
    name: &str,
    json: bool,
) -> anyhow::Result<()> {
    let url = format!("orgs/{org_slug}/instances/{name}/backups");
    let backups: Vec<Backup> = client.get(url).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&backups)?);
    } else {
        print_table(backups.into_iter());
    }

    Ok(())
}

fn print_table(items: impl Iterator<Item = Backup>) {
    let mut table = Table::new();
    table.set_format(*table::FORMAT);
    table.set_titles(Row::new(
        ["ID", "Created", "Type", "Status", "Server Version"]
            .iter()
            .map(|x| table::header_cell(x))
            .collect(),
    ));
    for key in items {
        table.add_row(Row::new(vec![
            Cell::new(&key.id),
            Cell::new(&humantime::format_rfc3339_seconds(key.created_on).to_string()),
            Cell::new(&key.r#type),
            Cell::new(&key.status),
            Cell::new(&key.edgedb_version),
        ]));
    }
    if !table.is_empty() {
        table.printstd();
    } else {
        println!("No backups found.")
    }
}
