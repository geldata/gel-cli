use crate::branch::connections::connect_if_branch_exists;
use crate::branch::context::Context;
use crate::commands::ExitCode;
use crate::connect::Connector;
use crate::portable::exit_codes;
use crate::{hooks, print, question};

pub async fn main(
    cmd: &Command,
    context: &Context,
    connector: &mut Connector,
) -> anyhow::Result<()> {
    let connection = connect_if_branch_exists(connector.branch(&cmd.target_branch)?).await?;

    if connection.is_none() {
        anyhow::bail!("Branch '{}' doesn't exist", &cmd.target_branch)
    }

    let mut connection = connection.unwrap();

    if !cmd.non_interactive {
        let q = question::Confirm::new_dangerous(format!(
            "Do you really want to wipe \
                    the contents of the branch {:?}?",
            cmd.target_branch
        ));
        if !connection.ping_while(q.async_ask()).await? {
            print::error!("Canceled by user.");
            return Err(ExitCode::new(exit_codes::NOT_CONFIRMED).into());
        }
    }

    do_wipe(&mut connection, context).await?;
    Ok(())
}

pub async fn do_wipe(
    connection: &mut crate::connect::Connection,
    context: &Context,
) -> Result<(), anyhow::Error> {
    if !context.skip_hooks() {
        if let Some(project) = context.get_project().await? {
            hooks::on_action("branch.wipe.before", &project).await?;
            hooks::on_action("schema.update.before", &project).await?;
        }
    }

    let (status, _warnings) = connection.execute("RESET SCHEMA TO initial", &()).await?;
    print::completion(status);

    if !context.skip_hooks() {
        if let Some(project) = context.get_project().await? {
            hooks::on_action("branch.wipe.after", &project).await?;
            hooks::on_action("schema.update.after", &project).await?;
        }
    }
    Ok(())
}

/// Wipes all data within a branch.
#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    /// The branch to wipe.
    pub target_branch: String,

    /// Wipe without asking for confirmation.
    #[arg(long)]
    pub non_interactive: bool,
}
