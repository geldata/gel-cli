use crate::connect::Connection;
use crate::print::{self};
use edgeql_parser::helpers::quote_string;
use gel_errors::DisabledCapabilityError;
use gel_protocol::model::Duration;

pub async fn inhibit_for_transaction(cli: &mut Connection) -> Result<Duration, anyhow::Error> {
    let old_timeout = cli
        .query_required_single::<Duration, _>(
            "SELECT assert_single(cfg::Config.session_idle_transaction_timeout)",
            &(),
        )
        .await?;
    cli.execute(
        "CONFIGURE SESSION SET session_idle_transaction_timeout \
             := <std::duration>'0'",
        &(),
    )
    .await
    .map(|_| ())
    .or_else(|e| {
        if e.is::<DisabledCapabilityError>() {
            print::warn!("Could not configure session_idle_transaction_timeout: {e}");
            Ok(())
        } else {
            Err(e)
        }
    })?;
    Ok(old_timeout)
}

pub async fn restore_for_transaction(
    cli: &mut Connection,
    old: Duration,
) -> Result<(), anyhow::Error> {
    if cli.is_consistent() {
        cli.execute(
            &format!(
                "CONFIGURE SESSION SET session_idle_transaction_timeout \
               := <std::duration>{}",
                quote_string(&old.to_string())
            ),
            &(),
        )
        .await
        .map(|_| ())
        .or_else(|e| {
            if e.is::<DisabledCapabilityError>() {
                Ok(())
            } else {
                Err(e)
            }
        })?;
    }
    Ok(())
}
