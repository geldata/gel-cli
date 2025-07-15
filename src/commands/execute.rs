use crate::connect::Connection;
use gel_tokio::server_params::{PostgresAddress, PostgresDsn};

use crate::analyze;
use crate::branch;
use crate::branding::BRANDING;
use crate::commands;
use crate::commands::Options;
use crate::commands::generate;
use crate::commands::parser::{Common, DatabaseCmd, DescribeCmd, ListCmd};
use crate::migrations;
use crate::migrations::options::MigrationCmd;
use crate::print;

pub async fn common(
    conn: Option<&mut Connection>,
    cmd: &Common,
    options: &Options,
) -> Result<branch::CommandResult, anyhow::Error> {
    use Common::*;

    // match commands that don't need connection
    if let Branch(cmd) = cmd {
        return Box::pin(branch::run(&cmd.subcommand, options, conn)).await;
    }

    // connect
    let mut conn_cell;
    let conn = if let Some(conn) = conn {
        conn
    } else {
        conn_cell = options.conn_params.connect().await?;
        &mut conn_cell
    };

    // match other
    match cmd {
        List(c) => match &c.subcommand {
            ListCmd::Aliases(c) => {
                commands::list_aliases(
                    conn,
                    options,
                    &c.pattern,
                    c.system,
                    c.case_sensitive,
                    c.verbose,
                )
                .await?;
            }
            ListCmd::Casts(c) => {
                commands::list_casts(conn, options, &c.pattern, c.case_sensitive).await?;
            }
            ListCmd::Indexes(c) => {
                commands::list_indexes(
                    conn,
                    options,
                    &c.pattern,
                    c.system,
                    c.case_sensitive,
                    c.verbose,
                )
                .await?;
            }
            ListCmd::Databases => {
                commands::list_databases(conn, options).await?;
            }
            ListCmd::Branches => {
                commands::list_branches(conn, options).await?;
            }
            ListCmd::Scalars(c) => {
                commands::list_scalar_types(conn, options, &c.pattern, c.system, c.case_sensitive)
                    .await?;
            }
            ListCmd::Types(c) => {
                commands::list_object_types(conn, options, &c.pattern, c.system, c.case_sensitive)
                    .await?;
            }
            ListCmd::Modules(c) => {
                commands::list_modules(conn, options, &c.pattern, c.case_sensitive).await?;
            }
            ListCmd::Roles(c) => {
                commands::list_roles(conn, options, &c.pattern, c.case_sensitive).await?;
            }
        },
        Analyze(c) => {
            analyze::command(conn, c).await?;
        }
        // generate doesn't *really* need a connection, but we treat
        // it as if it does so that we get the connection failed error
        // from the CLI instead of whichever binding.
        Generate(cmd) => {
            generate::run(cmd, options).await?;
        }
        Pgaddr => match conn.get_server_param::<PostgresAddress>() {
            Some(addr) => {
                // < 6.x
                println!("{}", serde_json::to_string_pretty(addr)?);
            }
            None => {
                // >= 6.x
                match conn.get_server_param::<PostgresDsn>() {
                    Some(addr) => {
                        println!("{}", addr.0);
                    }
                    None => print::error!("pgaddr requires {BRANDING} to run in DEV mode"),
                }
            }
        },
        Psql => {
            commands::psql(conn, options).await?;
        }
        Describe(c) => match &c.subcommand {
            DescribeCmd::Object(c) => {
                commands::describe(conn, options, &c.name, c.verbose).await?;
            }
            DescribeCmd::Schema(_) => {
                commands::describe_schema(conn, options).await?;
            }
        },
        Dump(c) => {
            commands::dump(conn, options, c).await?;
        }
        Restore(params) => {
            commands::restore(conn, options, params).await?;
        }
        Configure(cmd) => {
            commands::configure::run(cmd, conn, options).await?;
        }
        Database(c) => match &c.subcommand {
            DatabaseCmd::Create(c) => {
                commands::database::create(conn, c, options).await?;
            }
            DatabaseCmd::Drop(d) => {
                commands::database::drop(conn, d, options).await?;
            }
            DatabaseCmd::Wipe(w) => {
                commands::database::wipe(conn, w, options.skip_hooks).await?;
            }
        },
        Branch(_) => unreachable!(),
        Migrate(cmd) => {
            migrations::apply::run(cmd, conn, options, false).await?;
        }
        Migration(m) => match &m.subcommand {
            MigrationCmd::Apply(cmd) => {
                migrations::apply::run(cmd, conn, options, false).await?;
            }
            MigrationCmd::Create(cmd) => {
                migrations::create::run(cmd, conn, options).await?;
            }
            MigrationCmd::Status(cmd) => {
                migrations::status(conn, cmd, options).await?;
            }
            MigrationCmd::Log(cmd) => {
                migrations::log(conn, cmd, options).await?;
            }
            MigrationCmd::Edit(cmd) => {
                migrations::edit(conn, cmd, options).await?;
            }
            MigrationCmd::UpgradeCheck(_) => {
                anyhow::bail!("cannot be run in REPL mode");
            }
            MigrationCmd::Extract(cmd) => {
                migrations::extract(conn, cmd, options).await?;
            }
            MigrationCmd::UpgradeFormat(cmd) => {
                migrations::upgrade_format(conn, cmd, options).await?;
            }
        },
    }
    Ok(branch::CommandResult::default())
}
