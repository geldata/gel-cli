use crate::portable;

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
    /// Force WSL initialization
    #[command(hide = true)]
    __InitWsl(portable::windows::InitWslCommand),
}

pub fn run(cmd: &Command, opts: &crate::options::Options) -> anyhow::Result<()> {
    use Subcommand::*;

    match &cmd.subcommand {
        Upgrade(s) => upgrade::run(s),
        Install(s) => install::run(s, Some(opts)),
        __InitWsl(s) => portable::windows::init_wsl(s, opts),
    }
}
