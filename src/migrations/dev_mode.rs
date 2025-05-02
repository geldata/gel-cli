use crate::connect::Connection;
use indexmap::IndexMap;

use anyhow::Context as _;
use gel_errors::QueryError;
use indicatif::ProgressBar;
use std::sync::LazyLock;

use crate::async_try;
use crate::branding::BRANDING;
use crate::bug;
use crate::commands::Options;
use crate::migrations::apply::{apply_migrations, apply_migrations_inner};
use crate::migrations::context::Context;
use crate::migrations::create;
use crate::migrations::create::{CurrentMigration, FutureMigration};
use crate::migrations::create::{MigrationKey, write_migration};
use crate::migrations::create::{execute_start_migration, unsafe_populate};
use crate::migrations::create::{first_migration, normal_migration};
use crate::migrations::edb::{execute, execute_if_connected, query_row};
use crate::migrations::migration::{self, MigrationFile};
use crate::migrations::timeout;
use crate::portable::ver;

pub async fn migrate(cli: &mut Connection, ctx: &Context, bar: &ProgressBar) -> anyhow::Result<()> {
    if !check_client(cli).await? {
        anyhow::bail!(
            "Dev mode is not supported on {BRANDING} {}. Please upgrade.",
            cli.get_version().await?
        );
    }
    let migrations = migration::read_all(ctx, true).await?;
    let db_migration = get_db_migration(cli).await?;
    match select_mode(cli, &migrations, db_migration.as_deref()).await? {
        Mode::Normal { skip } => {
            log::info!("Skipping {} revisions.", skip);
            let migrations = migrations
                .get_range(skip..)
                .ok_or_else(|| bug::error("`skip` is out of range"))?;
            if !migrations.is_empty() {
                bar.set_message("Applying migrations");
                apply_migrations(cli, migrations, ctx, false).await?;
                bar.println("Migrations applied");
            }

            bar.set_message("Calculating diff");
            log::info!("Calculating schema diff");
            let applied_changes = migrate_to_schema(cli, ctx).await?;
            if applied_changes {
                bar.println("Changes applied.");
            } else {
                bar.println("Schema up to date.");
            }
        }
        Mode::Rebase => {
            bar.set_message("Calculating diff");
            log::info!("Calculating schema diff");
            let applied_changes = migrate_to_schema(cli, ctx).await?;

            log::info!("Now rebasing on top of filesystem migrations.");
            bar.set_message("Rebasing migrations");
            rebase_to_schema(cli, ctx, &migrations).await?;
            if applied_changes {
                bar.println("Migrations applied via rebase. There are pending --dev-mode changes.")
            } else {
                bar.println("Migrations applied via rebase.");
            }
        }
    }
    Ok(())
}

enum Mode {
    Normal { skip: usize },
    Rebase,
}

static MINIMUM_VERSION: LazyLock<ver::Build> =
    LazyLock::new(|| "3.0-alpha.1+05474ea".parse().unwrap());

mod ddl {
    // Just for nice log filter
    use super::{Connection, execute};

    pub async fn apply_statements(cli: &mut Connection, items: &[String]) -> anyhow::Result<()> {
        execute(
            cli,
            format!(
                "CREATE MIGRATION {{
                SET generated_by := schema::MigrationGeneratedBy.DevMode;
                {}
            }}",
                items.join("\n")
            ),
            None,
        )
        .await?;
        for ddl_statement in items {
            log::info!("{}", ddl_statement);
        }
        Ok(())
    }
}

pub async fn check_client(cli: &mut Connection) -> anyhow::Result<bool> {
    ver::check_client(cli, &MINIMUM_VERSION).await
}

async fn select_mode(
    cli: &mut Connection,
    migrations: &IndexMap<String, MigrationFile>,
    db_migration: Option<&str>,
) -> anyhow::Result<Mode> {
    if let Some(db_migration) = &db_migration {
        for (idx, (key, _)) in migrations.iter().enumerate() {
            if key == db_migration {
                return Ok(Mode::Normal { skip: idx + 1 });
            }
        }
        let last_fs_migration = migrations.last().map(|(id, _)| id.clone());
        if let Some(id) = last_fs_migration {
            let contains_last_fs_migration: bool = cli
                .query_required_single(
                    r###"
                    select exists(
                        select schema::Migration filter .name = <str>$0
                    )
                "###,
                    &(id,),
                )
                .await?;
            if contains_last_fs_migration {
                Ok(Mode::Normal {
                    skip: migrations.len(),
                })
            } else {
                Ok(Mode::Rebase)
            }
        } else {
            Ok(Mode::Normal {
                skip: migrations.len(), /* == 0 */
            })
        }
    } else {
        Ok(Mode::Normal { skip: 0 })
    }
}

async fn get_db_migration(cli: &mut Connection) -> anyhow::Result<Option<String>> {
    let (res, _) = cli
        .query_single(
            r###"
            WITH Last := (SELECT schema::Migration
                          FILTER NOT EXISTS .<parents[IS schema::Migration])
            SELECT name := assert_single(Last.name)
        "###,
            &(),
        )
        .await?;
    Ok(res)
}

async fn migrate_to_schema(cli: &mut Connection, ctx: &Context) -> anyhow::Result<bool> {
    use gel_protocol::server_message::TransactionState::NotInTransaction;

    let transaction = matches!(cli.transaction_state(), NotInTransaction);
    if transaction {
        execute(cli, "START TRANSACTION", None).await?;
    }
    async_try! {
        async {
            _migrate_to_schema(cli, ctx).await
        },
        finally async {
            if transaction {
                execute_if_connected(cli, "COMMIT").await?;
            }
            anyhow::Ok(())
        }
    }
}

async fn _migrate_to_schema(cli: &mut Connection, ctx: &Context) -> anyhow::Result<bool> {
    execute(cli, "DECLARE SAVEPOINT migrate_to_schema", None).await?;
    let descr = async_try! {
        async {
            execute_start_migration(ctx, cli).await?;
            if let Err(e) = execute(cli, "POPULATE MIGRATION", None).await {
                if e.is::<QueryError>() {
                    return Ok(None)
                } else {
                    return Err(e)?;
                }
            }
            let descr = query_row::<CurrentMigration>(cli,
                "DESCRIBE CURRENT MIGRATION AS JSON"
            ).await?;
            if !descr.complete {
                anyhow::bail!("Migration cannot be automatically populated");
            }
            Ok(Some(descr))
        },
        finally async {
            execute_if_connected(cli,
                "ROLLBACK TO SAVEPOINT migrate_to_schema",
            ).await?;
            execute_if_connected(cli,
                "RELEASE SAVEPOINT migrate_to_schema",
            ).await
        }
    }?;
    let descr = if let Some(descr) = descr {
        descr
    } else {
        execute_start_migration(ctx, cli).await?;
        async_try! {
            async {
                unsafe_populate(ctx, cli).await
            },
            finally async {
                execute_if_connected(cli, "ABORT MIGRATION",).await
            }
        }?
    };
    if !descr.confirmed.is_empty() {
        ddl::apply_statements(cli, &descr.confirmed).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub async fn rebase_to_schema(
    cli: &mut Connection,
    ctx: &Context,
    migrations: &IndexMap<String, MigrationFile>,
) -> anyhow::Result<()> {
    execute(cli, "START MIGRATION REWRITE", None).await?;

    let res = async {
        apply_migrations_inner(cli, migrations, false).await?;
        migrate_to_schema(cli, ctx).await?;
        Ok(())
    }
    .await;

    match res {
        Ok(()) => {
            execute(cli, "COMMIT MIGRATION REWRITE", None)
                .await
                .context("commit migration rewrite")?;
            Ok(())
        }
        Err(e) => {
            execute_if_connected(cli, "ABORT MIGRATION REWRITE")
                .await
                .map_err(|e| {
                    log::warn!("Error aborting migration rewrite: {:#}", e);
                })
                .ok();
            Err(e)
        }
    }
}

async fn create_in_rewrite(
    cmd: &create::Command,
    conn: &mut Connection,
    migrations: &IndexMap<String, MigrationFile>,
    ctx: &Context,
) -> anyhow::Result<FutureMigration> {
    apply_migrations_inner(conn, migrations, false).await?;
    if migrations.is_empty() {
        first_migration(conn, ctx, cmd).await
    } else {
        let key = MigrationKey::Index((migrations.len() + 1) as u64);
        let parent = migrations.keys().last().map(|x| &x[..]);
        normal_migration(conn, ctx, key, parent, cmd).await
    }
}

pub async fn create(
    cmd: &create::Command,
    conn: &mut Connection,
    _options: &Options,
    ctx: &Context,
) -> anyhow::Result<()> {
    let migrations = migration::read_all(ctx, true).await?;

    let old_timeout = timeout::inhibit_for_transaction(conn).await?;
    let migration = async_try! {
        async {
            execute(conn, "START MIGRATION REWRITE", None).await?;
            async_try! {
                async {
                    create_in_rewrite(cmd, conn,  &migrations, ctx,).await
                },
                finally async {
                    execute_if_connected(conn, "ABORT MIGRATION REWRITE").await
                        .context("migration rewrite cleanup")
                }
            }
        },
        finally async {
            timeout::restore_for_transaction(conn, old_timeout).await
        }
    }?;
    write_migration(ctx, &migration, !cmd.non_interactive).await?;
    Ok(())
}
