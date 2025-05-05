use crate::commands::Options;
use crate::connect::Connection;
use crate::migrations::context::Context;
use crate::migrations::options::MigrationLog;
use crate::migrations::{db_migration, migration};
use crate::print::Highlight;

pub async fn log(
    conn: &mut Connection,
    cmd: &MigrationLog,
    opts: &Options,
) -> Result<(), anyhow::Error> {
    if cmd.from_fs {
        log_fs_async(cmd, opts).await
    } else if cmd.from_db {
        return log_db(conn, opts, cmd).await;
    } else {
        anyhow::bail!("use either --from-fs or --from-db");
    }
}

pub async fn log_db(
    conn: &mut Connection,
    common: &Options,
    options: &MigrationLog,
) -> Result<(), anyhow::Error> {
    let old_state = conn.set_ignore_error_state();
    let res = _log_db(conn, common, options).await;
    conn.restore_state(old_state);
    res
}

async fn _log_db(
    conn: &mut Connection,
    _common: &Options,
    options: &MigrationLog,
) -> Result<(), anyhow::Error> {
    let migrations = db_migration::read_all(conn, false, false).await?;
    print(&migrations, options);
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn log_fs(cmd: &MigrationLog, opts: &Options) -> Result<(), anyhow::Error> {
    log_fs_async(cmd, opts).await
}

async fn log_fs_async(cmd: &MigrationLog, opts: &Options) -> Result<(), anyhow::Error> {
    assert!(cmd.from_fs);

    let ctx = Context::for_migration_config(&cmd.cfg, false, opts.skip_hooks).await?;
    let migrations = migration::read_all(&ctx, true).await?;
    print(&migrations, cmd);
    Ok(())
}

fn print<T>(migrations: &indexmap::IndexMap<String, T>, options: &MigrationLog) {
    let limit = options.limit.unwrap_or(migrations.len());
    if options.newest_first {
        for rev in migrations.keys().rev().take(limit) {
            println!("{rev}");
        }
    } else {
        for rev in migrations.keys().take(limit) {
            println!("{rev}");
        }
    }
    if migrations.is_empty() {
        println!("{}", "<no migrations>".muted());
    }
}
