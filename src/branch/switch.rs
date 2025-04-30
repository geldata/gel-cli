use crate::branch::connections::connect_if_branch_exists;
use crate::branch::context::Context;
use crate::branch::create::create_branch;
use crate::connect::Connector;
use crate::{branch, hooks, print};

pub async fn run(
    options: &Command,
    context: &Context,
    connector: &mut Connector,
) -> anyhow::Result<branch::CommandResult> {
    if !context.can_update_current_branch() {
        print::error!("Cannot switch branches without specifying the instance");
        print::msg!("Either change directory to a project or use --instance argument.");
        anyhow::bail!("");
    }

    if !context.skip_hooks() {
        if let Some(project) = &context.get_project().await? {
            hooks::on_action("branch.switch.before", project).await?;
            hooks::on_action("schema.update.before", project).await?;
        }
    }

    let current_branch = if let Some(mut connection) = connect_if_branch_exists(connector).await? {
        let current_branch = context.get_current_branch(&mut connection).await?;
        if current_branch == options.target_branch {
            anyhow::bail!("Already on '{}'", options.target_branch);
        }

        branch::verify_server_can_use_branches(&mut connection).await?;

        // verify the branch exists
        let branches: Vec<String> = connection
            .query(
                "SELECT (SELECT sys::Database FILTER NOT .builtin).name",
                &(),
            )
            .await?;

        if !branches.contains(&options.target_branch) {
            if options.create {
                eprintln!("Creating '{}'...", &options.target_branch);
                create_branch(
                    &mut connection,
                    &options.target_branch,
                    options.from.as_ref().unwrap_or(&current_branch),
                    options.empty,
                    options.copy_data,
                )
                .await?;
            } else {
                anyhow::bail!("Branch '{}' doesn't exist", options.target_branch)
            }
        }
        current_branch
    } else {
        // try to connect to the target branch
        let target_branch_connector = connector.branch(&options.target_branch)?;
        match connect_if_branch_exists(target_branch_connector).await? {
            Some(mut connection) => {
                branch::verify_server_can_use_branches(&mut connection).await?;

                context.get_current_branch(&mut connection).await?
            }
            None => anyhow::bail!("The target branch doesn't exist."),
        }
    };

    print::msg!(
        "Switching from '{}' to '{}'",
        current_branch,
        options.target_branch
    );

    context
        .update_current_branch(&options.target_branch)
        .await?;

    if !context.skip_hooks() {
        if let Some(project) = &context.get_project().await? {
            hooks::on_action("branch.switch.after", project).await?;
            hooks::on_action("schema.update.after", project).await?;
        }
    }

    Ok(branch::CommandResult {
        new_branch: Some(options.target_branch.clone()),
    })
}

/// Switch the current branch.
#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    /// The branch to switch to.
    pub target_branch: String,

    /// Create the branch if it doesn't exist.
    #[arg(short = 'c', long)]
    pub create: bool,

    /// If creating a new branch: whether the new branch should be empty.
    #[arg(short = 'e', long, conflicts_with = "copy_data")]
    pub empty: bool,

    /// If creating a new branch: the optional 'base' of the branch to create.
    #[arg(long)]
    pub from: Option<String>,

    /// If creating a new branch: whether to copy data from the 'base' branch.
    #[arg(alias = "cp", long)]
    pub copy_data: bool,
}
