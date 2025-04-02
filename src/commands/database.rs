use edgeql_parser::helpers::quote_name;

use crate::branding::BRANDING;
use crate::commands::parser::{CreateDatabase, DropDatabase, WipeDatabase};
use crate::commands::{ExitCode, Options};
use crate::connect::Connection;
use crate::hint::HintExt;
use crate::portable::exit_codes;
use crate::print;
use crate::question;

pub async fn create(
    cli: &mut Connection,
    options: &CreateDatabase,
    _: &Options,
) -> Result<(), anyhow::Error> {
    if cli.get_version().await?.specific().major >= 5 {
        eprintln!("'database create' is deprecated in {BRANDING} 5+. Please use 'branch create'");
    }

    let (status, _warnings) = cli
        .execute(
            &format!("CREATE DATABASE {}", quote_name(&options.database_name)),
            &(),
        )
        .await?;
    print::completion(&status);
    Ok(())
}

pub async fn drop(
    cli: &mut Connection,
    options: &DropDatabase,
    _: &Options,
) -> Result<(), anyhow::Error> {
    if cli.get_version().await?.specific().major >= 5 {
        eprintln!("'database drop' is deprecated in {BRANDING} 5+. Please use 'branch drop'");
    }

    if !options.non_interactive {
        let q = question::Confirm::new_dangerous(format!(
            "Do you really want to delete database {:?}?",
            options.database_name
        ));
        if !cli.ping_while(q.async_ask()).await? {
            print::error!("Canceled.");
            return Err(ExitCode::new(exit_codes::NOT_CONFIRMED).into());
        }
    }
    let (status, _warnings) = cli
        .execute(
            &format!("DROP DATABASE {}", quote_name(&options.database_name)),
            &(),
        )
        .await?;
    print::completion(&status);
    Ok(())
}

pub async fn wipe(connection: &mut Connection, cmd: &WipeDatabase) -> Result<(), anyhow::Error> {
    if connection.get_version().await?.specific().major >= 5 {
        print::warn!("'database wipe' is deprecated in {BRANDING} 5+. Please use 'branch wipe'");
    }

    if connection.get_version().await?.specific() < "3.0-alpha.2".parse().unwrap() {
        return Err(anyhow::anyhow!(
            "The `database wipe` command is only \
                            supported in {BRANDING} >= 3.0"
        ))
        .hint("Use `database drop`, `database create`")?;
    }
    if !cmd.non_interactive {
        let q = question::Confirm::new_dangerous(format!(
            "Do you really want to wipe \
                    the contents of the database {:?}?",
            connection.database()
        ));
        if !connection.ping_while(q.async_ask()).await? {
            print::error!("Canceled.");
            return Err(ExitCode::new(exit_codes::NOT_CONFIRMED).into());
        }
    }

    let context =
        crate::branch::context::Context::new(cmd.instance_opts.maybe_instance().as_ref(), cmd.branch.as_deref()).await?;
    crate::branch::wipe::do_wipe(connection, &context).await?;
    Ok(())
}
