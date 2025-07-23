use std::io::IsTerminal;

use crate::cli;
use crate::cloud::main::cloud_main;
use crate::commands;
use crate::commands::parser::Common;
use crate::migrations;
use crate::migrations::options::{Migration, MigrationCmd as M};
use crate::non_interactive;
use crate::options::{Command, Options};
use crate::portable;
use crate::print::style::Styler;
use crate::watch;

#[tokio::main(flavor = "current_thread")]
async fn common_cmd(
    _options: &Options,
    cmdopt: commands::Options,
    cmd: &Common,
) -> Result<(), anyhow::Error> {
    Box::pin(commands::execute::common(None, cmd, &cmdopt)).await?;
    Ok(())
}

pub fn main(options: &Options) -> Result<(), anyhow::Error> {
    match options.subcommand.as_ref().expect("subcommand is present") {
        Command::Common(cmd) => {
            let opts = init_command_opts(options)?;
            match cmd.as_migration() {
                // Process commands that don't need connection first
                Some(Migration {
                    subcommand: M::Log(cmd),
                    ..
                }) if cmd.from_fs => migrations::log_fs(cmd, &opts),
                Some(Migration {
                    subcommand: M::Edit(cmd),
                    ..
                }) if cmd.no_check => migrations::edit_no_check(cmd, &opts),
                Some(Migration {
                    subcommand: M::UpgradeCheck(params),
                    ..
                }) => migrations::upgrade_check(&opts, params),
                // Otherwise connect
                _ => common_cmd(options, opts, cmd),
            }
        }
        Command::Server(cmd) => portable::server::run(cmd),
        Command::Extension(cmd) => portable::extension::run(cmd, options),
        Command::Instance(cmd) => portable::instance::run(cmd, options),
        Command::Project(cmd) => portable::project::run(cmd, options),
        Command::Query(q) => non_interactive::noninteractive_main(q, options),
        Command::Init(cmd) => portable::project::init::run(cmd, options),
        Command::Sync(cmd) => portable::project::sync::run(cmd, options),
        Command::_SelfInstall(s) => cli::install::run(s, Some(options)),
        Command::_GenCompletions(s) => cli::gen_completions::run(s),
        Command::Cli(c) => cli::run(c, options),
        Command::Info(info) => commands::info(options, info),
        Command::UI(c) => commands::show_ui(c, options),
        Command::Cloud(c) => cloud_main(c, &options.cloud_options),
        Command::Watch(c) => watch::run(options, c),
        Command::HashPassword(cmd) => {
            println!("{}", portable::password_hash(&cmd.password_to_hash));
            Ok(())
        }
    }
}

fn init_command_opts(options: &Options) -> Result<commands::Options, anyhow::Error> {
    Ok(commands::Options {
        command_line: true,
        styler: if std::io::stdout().is_terminal() {
            Some(Styler::new())
        } else {
            None
        },
        instance_name: options.conn_options.instance_opts.maybe_instance(),
        conn_params: options.block_on_create_connector()?,
        skip_hooks: options.skip_hooks,
    })
}
