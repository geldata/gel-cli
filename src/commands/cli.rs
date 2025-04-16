use std::io::IsTerminal;

use crate::cli;
use crate::cli::directory_check;
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
    commands::execute::common(None, cmd, &cmdopt).await?;
    Ok(())
}

pub fn main(options: &Options) -> Result<(), anyhow::Error> {
    match options.subcommand.as_ref().expect("subcommand is present") {
        Command::Common(cmd) => {
            let cmdopt = init_command_opts(options)?;
            directory_check::check_and_warn();
            match cmd.as_migration() {
                // Process commands that don't need connection first
                Some(Migration {
                    subcommand: M::Log(mlog),
                    ..
                }) if mlog.from_fs => migrations::log_fs(&cmdopt, mlog),
                Some(Migration {
                    subcommand: M::Edit(params),
                    ..
                }) if params.no_check => migrations::edit_no_check(&cmdopt, params),
                Some(Migration {
                    subcommand: M::UpgradeCheck(params),
                    ..
                }) => migrations::upgrade_check(&cmdopt, params),
                // Otherwise connect
                _ => common_cmd(options, cmdopt, cmd),
            }
        }
        Command::Server(cmd) => {
            directory_check::check_and_error()?;
            portable::server::run(cmd)
        }
        Command::Extension(cmd) => {
            directory_check::check_and_error()?;
            portable::extension::run(cmd, options)
        }
        Command::Instance(cmd) => {
            directory_check::check_and_error()?;
            portable::instance::run(cmd, options)
        }
        Command::Init(cmd) => {
            directory_check::check_and_error()?;
            portable::project::init::run(cmd, options)
        }
        Command::Project(cmd) => {
            directory_check::check_and_error()?;
            portable::project::run(cmd, options)
        }
        Command::Query(q) => {
            directory_check::check_and_warn();
            non_interactive::noninteractive_main(q, options)
        }
        Command::_SelfInstall(s) => cli::install::run(s),
        Command::_GenCompletions(s) => cli::gen_completions::run(s),
        Command::Cli(c) => cli::run(c),
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
    })
}
