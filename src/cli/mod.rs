pub mod env;
pub mod gen_completions;
pub mod install;
pub mod logo;
pub mod upgrade;

#[macro_use]
mod markdown;

#[derive(clap::Args, Clone, Debug)]
#[command(version = "help_expand")]
#[command(disable_version_flag = true)]
pub struct Command {
    #[command(subcommand)]
    pub subcommand: Subcommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum Subcommand {
    /// Upgrade the [`BRANDING_CLI_CMD`] command-line tool
    Upgrade(upgrade::Command),
    /// Install the [`BRANDING_CLI_CMD`] command-line tool
    #[command(hide = true)]
    Install(install::Command),
}

pub fn run(cmd: &Command) -> anyhow::Result<()> {
    use Subcommand::*;

    match &cmd.subcommand {
        Upgrade(s) => upgrade::run(s),
        Install(s) => install::run(s),
    }
}
