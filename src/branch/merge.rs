use crate::branch::connections::connect_if_branch_exists;
use crate::branch::context::Context;
use crate::commands::Options;
use crate::connect::Connection;
use crate::migrations;
use crate::migrations::merge::{
    apply_merge_migration_files, get_merge_migrations, write_merge_migrations,
};

pub async fn main(
    cmd: &Command,
    context: &Context,
    source_connection: &mut Connection,
    opts: &Options,
) -> anyhow::Result<()> {
    let current_branch = context.get_current_branch(source_connection).await?;
    let project = context
        .get_project()
        .await?
        .ok_or_else(|| anyhow::anyhow!("Merge must be used within a project"))?;

    if cmd.target_branch == current_branch {
        anyhow::bail!("Cannot merge the current branch into its self");
    }

    let mut connector = opts.conn_params.clone();
    let mut target_connection =
        match connect_if_branch_exists(connector.branch(&cmd.target_branch)?).await? {
            Some(connection) => connection,
            None => anyhow::bail!("The branch '{}' doesn't exist", cmd.target_branch),
        };

    let migration_context = migrations::Context::for_project(project, opts.skip_hooks)?;
    let mut merge_migrations =
        get_merge_migrations(source_connection, &mut target_connection).await?;

    eprintln!(
        "Merging {} migration(s) into '{}'...",
        merge_migrations.target_migrations.len(),
        source_connection.database()
    );

    write_merge_migrations(&migration_context, &mut merge_migrations).await?;

    if !cmd.no_apply {
        eprintln!("Applying migrations...");
        apply_merge_migration_files(&merge_migrations, &migration_context, source_connection)
            .await?;
    }

    eprintln!("Done!");

    Ok(())
}

/// Merges a branch into this one via a fast-forward merge.
#[derive(clap::Args, Clone, Debug)]
pub struct Command {
    /// The branch to merge into this one.
    pub target_branch: String,

    /// Skip applying migrations generated from the merge.
    #[arg(long)]
    pub no_apply: bool,
}
