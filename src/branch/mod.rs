mod connections;
pub mod context;
pub mod create;
pub mod current;
pub mod drop;
pub mod list;
pub mod merge;
pub mod rebase;
pub mod rename;
pub mod switch;
pub mod wipe;

use crate::branding::BRANDING;
use crate::commands::Options;
use crate::connect::{Connection, Connector};
use crate::options::ConnectionOptions;

pub async fn run(
    cmd: &Subcommand,
    options: &Options,
    conn: Option<&mut Connection>,
) -> anyhow::Result<CommandResult> {
    let context = context::Context::new(options.instance_name.as_ref(), options.skip_hooks).await?;

    let mut connector: Connector = options.conn_params.clone();

    // commands that don't need connection
    match &cmd {
        Subcommand::Switch(switch) => return switch::run(switch, &context, &mut connector).await,
        Subcommand::Wipe(wipe) => {
            wipe::main(wipe, &context, &mut connector).await?;
            return Ok(CommandResult::default());
        }
        _ => {}
    }

    // connect
    let mut conn_cell;
    let conn = if let Some(conn) = conn {
        conn
    } else {
        conn_cell = options.conn_params.connect().await?;
        &mut conn_cell
    };

    verify_server_can_use_branches(conn).await?;

    match cmd {
        Subcommand::Current(cmd) => current::run(cmd, &context, conn).await?,
        Subcommand::Create(cmd) => create::run(cmd, &context, conn).await?,
        Subcommand::Drop(cmd) => drop::main(cmd, &context, conn).await?,
        Subcommand::List(cmd) => list::main(cmd, &context, conn).await?,
        Subcommand::Rename(cmd) => return rename::run(cmd, &context, conn, options).await,
        Subcommand::Rebase(cmd) => Box::pin(rebase::main(cmd, &context, conn, options)).await?,
        Subcommand::Merge(cmd) => merge::main(cmd, &context, conn, options).await?,

        // handled earlier
        Subcommand::Switch(_) | Subcommand::Wipe(_) => unreachable!(),
    }

    Ok(CommandResult::default())
}

#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    #[command(subcommand)]
    pub subcommand: Subcommand,
}

#[derive(Default)]
pub struct CommandResult {
    pub new_branch: Option<String>,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum Subcommand {
    Create(create::Command),
    Switch(switch::Command),
    List(list::Command),
    Current(current::Command),
    Rebase(rebase::Command),
    Merge(merge::Command),
    Rename(rename::Command),
    Drop(drop::Command),
    Wipe(wipe::Command),
}

pub async fn verify_server_can_use_branches(connection: &mut Connection) -> anyhow::Result<()> {
    let server_version = connection.get_version().await?;
    if server_version.specific().major < 5 {
        anyhow::bail!(
            "Branches are not supported on server version {}, please upgrade to {BRANDING} 5+",
            server_version
        );
    }

    Ok(())
}
