use crate::branding::BRANDING;
use crate::commands::Options;
use crate::commands::list;
use crate::commands::list_branches::list_branches0;
use crate::connect::Connection;
use crate::print;

pub async fn get_databases(cli: &mut Connection) -> anyhow::Result<Vec<String>> {
    let databases = cli
        .query(
            "SELECT (SELECT sys::Database FILTER NOT .builtin).name",
            &(),
        )
        .await?;
    Ok(databases)
}

pub async fn list_databases(cli: &mut Connection, options: &Options) -> Result<(), anyhow::Error> {
    let version = cli.get_version().await?;

    if version.specific().major >= 5 {
        print::warn!(
            "Databases are not supported in {BRANDING} {version}, printing list of branches instead"
        );
        return list_branches0(cli, options).await;
    }

    let databases = get_databases(cli).await?;
    list::print(databases, "List of databases", options).await?;
    Ok(())
}
