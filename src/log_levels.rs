use crate::cli;
use crate::commands::parser::Common;
use crate::instance;
use crate::migrations::options::MigrationCmd;
use crate::options::{Command, Options};
use crate::portable::server::Subcommands as Server;
use crate::project::Subcommands as Project;
use std::io::Write;

pub fn init(builder: &mut env_logger::Builder, opt: &Options) {
    if opt.debug_print_frames {
        builder.filter_module("edgedb::incoming::frame", log::LevelFilter::Debug);
    }
    match &opt.subcommand {
        Some(Command::Cli(c)) => match &c.subcommand {
            cli::Subcommand::Upgrade(s) if s.verbose => {
                builder.filter_module("edgedb::self_upgrade", log::LevelFilter::Info);
            }
            _ => {}
        },
        Some(Command::Common(Common::Restore(r))) if r.verbose => {
            builder.filter_module("edgedb::restore", log::LevelFilter::Info);
            builder.format(|buf, record| writeln!(buf, "{}", record.args()));
        }
        Some(Command::Watch(w)) if w.verbose => {
            builder.filter_module("edgedb::migrations::dev_mode::ddl", log::LevelFilter::Info);
        }
        Some(Command::Common(Common::Migration(c))) => match &c.subcommand {
            MigrationCmd::Create(c) if c.debug_print_queries => {
                builder.filter_module("edgedb::migrations::query", log::LevelFilter::Debug);
            }
            _ => {}
        },
        Some(Command::Server(s)) => match &s.subcommand {
            Server::Uninstall(u) if u.verbose => {
                builder.filter_module("edgedb::portable::uninstall", log::LevelFilter::Info);
            }
            _ => {}
        },
        Some(Command::Instance(i)) => match &i.subcommand {
            instance::Subcommands::Destroy(d) if d.verbose => {
                builder.filter_module("edgedb::portable::destroy", log::LevelFilter::Info);
            }
            instance::Subcommands::Upgrade(u) if u.verbose => {
                builder.filter_module("edgedb::portable::upgrade", log::LevelFilter::Info);
            }
            _ => {}
        },
        Some(Command::Project(p)) => match &p.subcommand {
            Project::Upgrade(u) if u.verbose => {
                builder.filter_module("edgedb::project::upgrade", log::LevelFilter::Info);
                builder.filter_module("edgedb::portable::upgrade", log::LevelFilter::Info);
                builder.filter_module("edgedb::portable::project", log::LevelFilter::Info);
            }
            _ => {}
        },
        _ => {}
    }
    // we have custom logging infrastructure for edgedb warnings
    builder.filter_module("gel_tokio::warning", log::LevelFilter::Error);
}
