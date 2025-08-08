use gel_errors::{ClientConnectionEosError, Error, ErrorKind, NoDataError};
use gel_protocol::queryable::Queryable;

use crate::connect::Connection;

use super::create::SourceName;
use super::source_map::SourceMap;

pub async fn execute(
    cli: &mut Connection,
    text: impl AsRef<str>,
    source_map: Option<&SourceMap<SourceName>>,
) -> Result<(), Error> {
    if !cli.is_consistent() {
        return Err(ClientConnectionEosError::with_message(
            "connection closed by server",
        ));
    }
    log_execute(cli, text, source_map).await
}

pub async fn execute_if_connected(
    cli: &mut Connection,
    text: impl AsRef<str>,
) -> Result<(), Error> {
    if !cli.is_consistent() {
        return Ok(());
    }
    log_execute(cli, text, None).await
}

async fn log_execute(
    cli: &mut Connection,
    text: impl AsRef<str>,
    source_map: Option<&SourceMap<SourceName>>,
) -> Result<(), Error> {
    let text = text.as_ref();
    log::debug!(target: "edgedb::migrations::query", "Executing `{text}`");
    let (_status, warnings) = cli.execute(text, &()).await?;
    super::print_error::print_warnings(warnings, source_map)?;
    Ok(())
}

pub async fn query_row<R>(cli: &mut Connection, text: &str) -> Result<R, Error>
where
    R: Queryable,
{
    log::debug!(target: "edgedb::migrations::query", "Executing `{text}`");
    let (data, _warnings) = cli.query_single(text, &()).await?;
    data.ok_or_else(|| NoDataError::with_message("query row returned zero results"))
}
