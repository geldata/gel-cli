use crate::branch;
use crate::branch::connections::get_connection_to_modify;
use crate::branch::context::Context;
use crate::commands::Options;
use crate::connect::Connection;
use crate::print;

pub async fn run(
    options: &Command,
    context: &Context,
    connection: &mut Connection,
    cli_opts: &Options,
) -> anyhow::Result<branch::CommandResult> {
    let current_branch = context.get_current_branch(connection).await?;

    let rename_current = options.old_name == current_branch
        || connection
            .database()
            .branch_for_connect()
            .unwrap_or_default()
            == options.old_name;

    if rename_current {
        let mut modify_connection =
            get_connection_to_modify(&options.old_name, cli_opts, connection).await?;
        rename(&mut modify_connection.connection, options).await?;
        modify_connection.clean().await?;
        context.update_current_branch(&options.new_name).await?;
    } else {
        rename(connection, options).await?;
    }

    eprintln!(
        "Renamed branch {} to {}",
        options.old_name, options.new_name
    );

    if rename_current {
        return Ok(branch::CommandResult {
            new_branch: Some(options.new_name.clone()),
        });
    }

    Ok(branch::CommandResult::default())
}

async fn rename(connection: &mut Connection, cmd: &Command) -> anyhow::Result<()> {
    let (status, _warnings) = connection
        .execute(
            &format!(
                "alter branch {0}{2} rename to {1}",
                edgeql_parser::helpers::quote_name(&cmd.old_name),
                edgeql_parser::helpers::quote_name(&cmd.new_name),
                if cmd.force { " force" } else { "" }
            ),
            &(),
        )
        .await?;

    print::completion(status);

    Ok(())
}

/// Renames a branch.
#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    /// The branch to rename.
    pub old_name: String,

    /// The new name of the branch.
    pub new_name: String,

    /// Close any existing connection to the branch before renaming it.
    #[arg(long)]
    pub force: bool,
}
