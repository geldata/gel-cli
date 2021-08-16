use colorful::Colorful;
use edgedb_client::client::Connection;
use edgedb_protocol::value::Value;

use crate::commands::{Options, ExitCode};
use crate::commands::parser::ShowStatus;
use crate::migrations::context::Context;
use crate::migrations::create::{execute_start_migration, CurrentMigration};
use crate::migrations::migration;
use crate::print;


async fn ensure_diff_is_empty(cli: &mut Connection, status: &ShowStatus)
    -> Result<(), anyhow::Error>
{
    let data = cli.query_row::<CurrentMigration>(
        "DESCRIBE CURRENT MIGRATION AS JSON",
        &Value::empty_tuple(),
    ).await?;
    if !data.confirmed.is_empty() || !data.complete {
        if !status.quiet {
            eprintln!("Detected differences between \
                the database schema and the schema source, \
                in particular:");
            let changes = data.confirmed.iter()
                .chain(data.proposed.iter()
                    .flat_map(|p| p.statements.iter().map(|s| &s.text)));
            for text in changes.take(3) {
                eprintln!("    {}",
                    text.lines().collect::<Vec<_>>()
                    .join("\n    "));
            }
            let changes = data.confirmed.len() +
                data.proposed.map(|_| 1).unwrap_or(0);
            if changes > 3 {
                eprintln!("... and {} more changes", changes - 3);
            }
            print::error_msg("Error", "Some migrations are missing.");
            eprintln!("  Use `edgedb migration create`.");
        }
        return Err(ExitCode::new(2).into());
    }
    Ok(())
}

pub async fn status(cli: &mut Connection, _options: &Options,
    status: &ShowStatus)
    -> Result<(), anyhow::Error>
{
    let ctx = Context::from_config(&status.cfg);
    let migrations = migration::read_all(&ctx, true).await?;
    let db_migration: Option<String> = cli.query_row_opt(r###"
            WITH Last := (SELECT schema::Migration
                          FILTER NOT EXISTS .<parents[IS schema::Migration])
            SELECT name := Last.name
        "###, &Value::empty_tuple()).await?;
    if db_migration.as_ref() != migrations.keys().last() {
        if !status.quiet {
            if let Some(db_migration) = &db_migration {
                if let Some(_) = migrations.get(db_migration) {
                    let mut iter = migrations.keys()
                        .skip_while(|k| k != &db_migration);
                    iter.next(); // skip db_migration itself
                    let first = iter.next().unwrap();  // we know it's not last
                    let count = iter.count() + 1;
                    print::error_msg("Error", &format!(
                        "Database is at migration {db:?} while sources \
                        contain {n} migrations ahead, \
                        starting from {first:?}({first_file})",
                        db=db_migration,
                        n=count,
                        first=first,
                        first_file=migrations[first].path.display())
                    );
                } else {
                    print::error_msg("Error", &format!(
                        "There is no database revision {} in the filesystem.",
                        db_migration)
                    );
                    eprintln!("  Consider updating sources.");
                }
            } else {
                print::error_msg("Error", &format!(
                    "Database is empty. While there are {} migrations \
                    on the filesystem.",
                    migrations.len())
                );
                eprintln!("  Run `edgedb migrate` to apply.");
            }
        }
        return Err(ExitCode::new(3).into());
    }
    execute_start_migration(&ctx, cli).await?;
    let check = ensure_diff_is_empty(cli, status).await;
    let abort = cli.execute("ABORT MIGRATION").await.map_err(|e| e.into());
    check.and(abort)?;
    if !status.quiet {
        eprintln!(
            "{} Last migration: {}.",
            "Database is up to date.".bold().light_green(),
            db_migration
                .as_ref()
                .map(|x| &x[..])
                .unwrap_or("initial")
                .bold()
                .white()
        );
    }
    Ok(())
}
