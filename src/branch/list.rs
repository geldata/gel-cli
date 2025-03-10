use crate::branch::context::Context;
use crate::connect::Connection;
use termimad::crossterm::style::Stylize;

pub async fn main(
    _options: &Command,
    context: &Context,
    connection: &mut Connection,
) -> anyhow::Result<()> {
    let current_branch = context.get_current_branch(connection).await?;

    let branches: Vec<String> = connection
        .query(
            "SELECT (SELECT sys::Database FILTER NOT .builtin).name",
            &(),
        )
        .await?;

    for branch in branches {
        if current_branch == branch {
            println!("{} - Current", branch.green());
        } else {
            println!("{branch}");
        }
    }

    Ok(())
}

/// List all branches.
#[derive(clap::Args, Debug, Clone)]
pub struct Command {}
