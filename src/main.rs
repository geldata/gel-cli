// We don't need to hunt of unused imports on windows, as they are harmless
#![cfg_attr(windows, allow(unused_imports))]
#![type_length_limit = "8388608"]

use clap::Parser;

use std::env;
use std::path::Path;
use std::process::exit;

use crate::branding::BRANDING;
use crate::options::{Options, UsageError};

mod analyze;
mod async_util;
mod branch;
mod branding;
mod browser;
mod bug;
mod classify;
pub(crate) mod cli;
mod cloud;
mod collect;
mod commands;
mod completion;
mod config;
mod connect;
mod credentials;
mod docker;
mod error_display;
mod format;
mod git;
mod highlight;
mod hint;
mod hooks;
mod interactive;
mod interrupt;
mod locking;
mod log_levels;
mod markdown;
mod migrations;
mod non_interactive;
mod options;
mod outputs;
mod platform;
mod portable;
mod print;
mod process;
mod project;
mod prompt;
mod question;
mod repl;
mod statement;
mod table;
mod tty_password;
mod variables;
mod version_check;
mod watch;
mod instance;


fn main() {
    match _main() {
        Ok(()) => {}
        Err(ref e) => {
            let mut err = e;
            let mut code = 1;
            if let Some(e) = err.downcast_ref::<commands::ExitCode>() {
                e.exit();
            }
            if let Some(e) = err.downcast_ref::<UsageError>() {
                e.exit();
            }
            if let Some(arc) = err.downcast_ref::<hint::ArcError>() {
                // prevent duplicate error message
                err = arc.inner();
            }
            if let Some(e) = err.downcast_ref::<gel_errors::Error>() {
                print::gel_error(e, false);
            } else {
                let mut error_chain = err.chain();
                if let Some(first) = error_chain.next() {
                    print::error!("{first}");
                } else {
                    print::error!(" <empty error message>");
                }
                for e in error_chain {
                    eprintln!("  Caused by: {e}");
                }
            }
            for item in err.chain() {
                if let Some(e) = item.downcast_ref::<hint::HintedError>() {
                    eprintln!(
                        "  Hint: {}",
                        e.hint.lines().collect::<Vec<_>>().join("\n        ")
                    );
                } else if item.is::<bug::Bug>() {
                    eprintln!(
                        "  Hint: This is most likely a bug in {BRANDING} \
                        or command-line tools. Please consider opening an \
                        issue at \
                        https://github.com/edgedb/edgedb-cli/issues/new\
                        ?template=bug_report.md"
                    );
                    code = 13;
                } else if let Some(e) = e.downcast_ref::<commands::ExitCode>() {
                    code = e.code();
                }
            }
            exit(code);
        }
    }
}

fn is_cli_upgrade(cmd: &Option<options::Command>) -> bool {
    use options::Command::Cli;
    matches!(
        cmd,
        Some(Cli(cli::Command {
            subcommand: cli::Subcommand::Upgrade(..)
        }))
    )
}

fn is_cli_self_install(cmd: &Option<options::Command>) -> bool {
    use options::Command::_SelfInstall;
    matches!(cmd, Some(_SelfInstall(..)))
}

fn _main() -> anyhow::Result<()> {
    // If a crash happens we want the backtrace to be printed by default
    // to ease bug reporting and troubleshooting.
    // TODO: consider removing this once EdgeDB reaches 1.0 stable.
    unsafe {
        env::set_var("RUST_BACKTRACE", "1");
    }
    interrupt::init_signals();

    if let Some(arg0) = std::env::args_os().next() {
        if let Some(exe_name) = Path::new(&arg0).file_name() {
            if exe_name.to_string_lossy().contains("-init") {
                let cmd = cli::install::Command::parse();
                return cli::install::run(&cmd, None);
            }
        }
    }

    // Determine the terminal background color, which will influence
    // various printing decisions. (We could let it be done lazily,
    // but I'd rather it be clear when it is run.)
    print::init_colors();

    let opt = Options::from_args_and_env()?;
    opt.conn_options.validate()?;
    let cfg = config::get_config();

    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"));
    log_levels::init(&mut builder, &opt);
    builder.init();

    let cfg = cfg.unwrap_or_else(|e| {
        log::warn!("Config error: {:#}", e);
        Default::default()
    });

    // Check the executable name and warn on older names, but not for self-install.
    if !is_cli_self_install(&opt.subcommand) && cfg!(feature = "gel") {
        cli::install::check_executables();
    }

    if !is_cli_upgrade(&opt.subcommand) {
        version_check::check(opt.no_cli_update_check)?;
    }

    if portable::windows::is_in_wsl1() {
        warn!(
            "WSL1 is unsupported. Please back up your {BRANDING} instances, remove all WSL distros, and upgrade your WSL installation to WSL2."
        );
    }

    if opt.subcommand.is_some() {
        commands::cli::main(&opt)
    } else {
        if opt.test_output_conn_params {
            println!("{}", opt.block_on_create_connector()?.get()?.to_json());
            return Ok(());
        }
        if opt.interactive {
            interactive::main(opt, cfg)
        } else {
            non_interactive::interpret_stdin(
                &opt,
                opt.output_format.unwrap_or(repl::OutputFormat::JsonPretty),
                opt.input_language.unwrap_or(repl::InputLanguage::EdgeQl),
            )
        }
    }
}
