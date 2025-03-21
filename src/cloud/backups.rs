use gel_cli_instance::cloud::{Backup, CloudInstanceRestore, CloudOperation};

use crate::table::{self, Cell, Row, Table};

use crate::cloud::client::CloudClient;
use crate::cloud::ops::wait_for_operation;

#[tokio::main(flavor = "current_thread")]
pub async fn backup_cloud_instance(
    client: &CloudClient,
    org: &str,
    name: &str,
) -> anyhow::Result<()> {
    let operation = client.api.create_backup(org, name).await?;
    wait_for_operation(operation, client).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn restore_cloud_instance(
    client: &CloudClient,
    org_slug: &str,
    name: &str,
    latest: bool,
    backup_id: Option<String>,
    source_instance_id: Option<String>,
) -> anyhow::Result<()> {
    let operation: CloudOperation = client
        .api
        .restore_instance(
            org_slug,
            name,
            CloudInstanceRestore {
                backup_id,
                latest,
                source_instance_id,
            },
        )
        .await?;
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
    let backups: Vec<Backup> = client.api.list_backups(org_slug, name).await?;

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
