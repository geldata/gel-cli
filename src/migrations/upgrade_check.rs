use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use gel_tokio::Builder;
use indicatif::ProgressBar;
use notify::RecursiveMode;
use tokio::fs;

use crate::async_try;
use crate::branding::{BRANDING_CLI_CMD, QUERY_TAG};
use crate::commands::{ExitCode, Options};
use crate::connect::Connection;
use crate::migrations::apply::{ApplyMigrationError, apply_migration};
use crate::migrations::context::Context;
use crate::migrations::create::{SchemaFileError, execute_start_migration};
use crate::migrations::edb::{execute, execute_if_connected};
use crate::migrations::migration;
use crate::migrations::options::UpgradeCheck;
use crate::migrations::timeout;
use crate::portable::local::InstallInfo;
use crate::portable::project;
use crate::portable::repository::{self, PackageInfo, Query};
use crate::portable::server::install;
use crate::print::{self, Highlight, msg};
use crate::process;
use crate::watch::{self, WatchOptions};

#[derive(Debug, serde::Deserialize)]
struct EdgedbStatus {
    port: u16,
    tls_cert_file: String,
}

enum CheckResult {
    Okay,
    SchemaIssue,
    MigrationsIssue,
}

#[cfg(windows)]
pub fn upgrade_check(_options: &Options, options: &UpgradeCheck) -> anyhow::Result<()> {
    use crate::portable::windows;

    let status_path = tempfile::NamedTempFile::new()
        .context("tempfile failure")?
        .into_temp_path();

    let mut cmd = windows::ensure_wsl()?.cli_exe();
    cmd.arg("migration").arg("upgrade-check");
    cmd.args(&UpgradeCheck {
        run_server_with_status: Some(windows::path_to_linux(&status_path)?.into()),
        ..options.clone()
    });
    cmd.background_for(move || {
        Ok(async move {
            while let Ok(meta) = fs::metadata(&status_path).await {
                if meta.len() > "READY={}".len() as u64 {
                    break;
                }
            }
            let ctx = Context::for_migration_config(&options.cfg, false, true).await?;

            Box::pin(do_check(
                &ctx,
                &status_path,
                options.watch,
                WatchOptions::default(),
            ))
            .await
        })
    })
}

#[cfg(unix)]
pub fn upgrade_check(_options: &Options, options: &UpgradeCheck) -> anyhow::Result<()> {
    use const_format::concatcp;

    use crate::branding::BRANDING;

    let (version, _) = Query::from_options(
        repository::QueryOptions {
            nightly: options.to_nightly,
            stable: false,
            testing: options.to_testing,
            version: options.to_version.as_ref(),
            channel: options.to_channel,
        },
        || Ok(Query::stable()),
    )?;

    let pkg = repository::get_server_package(&version)?
        .with_context(|| format!("no package matching {} found", version.display()))?;
    let info = install::package(&pkg).context(concatcp!("error installing ", BRANDING))?;

    // This is run from windows to do the upgrade check
    if let Some(status_path) = &options.run_server_with_status {
        let server_path = info.server_path()?;
        let mut cmd = process::Native::new("edgedb", "edgedb", server_path);
        cmd.arg("--temp-dir");
        cmd.arg("--auto-shutdown-after=0");
        cmd.arg("--default-auth-method=Trust");
        cmd.arg("--emit-server-status").arg(status_path);
        cmd.arg("--port=auto");
        cmd.arg("--compiler-pool-mode=on_demand");
        cmd.arg("--tls-cert-mode=generate_self_signed");
        cmd.arg("--log-level=warn");
        cmd.exec_replacing_self()?;
        unreachable!();
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let ctx = runtime.block_on(Context::for_migration_config(&options.cfg, false, true))?;

    let mut watch_options = WatchOptions::default();
    if !options.no_exit_with_parent {
        watch_options.exit_with_parent = true;
    }
    spawn_and_check(&info, ctx, options.watch, watch_options)
}

#[cfg(windows)]
pub fn to_version(_: &PackageInfo, _: &project::Context) -> anyhow::Result<()> {
    unreachable!();
}

#[cfg(unix)]
pub fn to_version(pkg: &PackageInfo, project: &project::Context) -> anyhow::Result<()> {
    use const_format::concatcp;

    use crate::branding::BRANDING;

    let info = install::package(pkg).context(concatcp!("error installing ", BRANDING))?;
    let ctx = Context::for_project(project.clone(), true)?;
    spawn_and_check(&info, ctx, false, WatchOptions::default())
}

#[cfg(unix)]
fn spawn_and_check(
    info: &InstallInfo,
    ctx: Context,
    watch: bool,
    watch_options: WatchOptions,
) -> anyhow::Result<()> {
    use tokio::net::UnixDatagram;

    let server_path = info.server_path()?;
    let status_dir = tempfile::tempdir().context("tempdir failure")?;
    let mut cmd = process::Native::new("edgedb", "edgedb", server_path);
    cmd.env("NOTIFY_SOCKET", status_dir.path().join("notify"));
    cmd.quiet();
    cmd.arg("--temp-dir");
    cmd.arg("--auto-shutdown-after=0");
    cmd.arg("--default-auth-method=Trust");
    cmd.arg("--emit-server-status")
        .arg(status_dir.path().join("status"));
    cmd.arg("--port=auto");
    cmd.arg("--compiler-pool-mode=on_demand");
    cmd.arg("--tls-cert-mode=generate_self_signed");
    cmd.arg("--log-level=warn");
    cmd.background_for(move || {
        // this is not async, but requires async context
        let sock = UnixDatagram::bind(status_dir.path().join("notify"))
            .context("cannot create notify socket")?;
        Ok(async move {
            let mut buf = [0u8; 1024];
            while !matches!(sock.recv(&mut buf).await,
                           Ok(len) if &buf[..len] == b"READY=1")
            {}

            let status_file = status_dir.path().join("status");
            do_check(&ctx, &status_file, watch, watch_options).await
        })
    })
}

async fn do_check(
    ctx: &Context,
    status_file: &Path,
    watch: bool,
    watch_options: WatchOptions,
) -> anyhow::Result<()> {
    use CheckResult::*;

    let status_data = fs::read_to_string(&status_file)
        .await
        .context("error reading status")?;
    let Some(json_data) = status_data.strip_prefix("READY=") else {
        anyhow::bail!("Invalid server status {status_data:?}");
    };
    let status: EdgedbStatus = serde_json::from_str(json_data)?;
    let cert_path = if cfg!(windows) {
        crate::portable::windows::path_to_windows(Path::new(&status.tls_cert_file))?
    } else {
        Path::new(&status.tls_cert_file).to_path_buf()
    };
    let config = Builder::new()
        .port(status.port)
        .tls_ca_file(&cert_path)
        .without_system()
        .with_fs()
        .build()
        .context("cannot build connection params")?;
    let cli = &mut Connection::connect(&config, QUERY_TAG).await?;

    if fs::metadata(&ctx.schema_dir).await.is_err() {
        anyhow::bail!("No schema dir found at {:?}", ctx.schema_dir);
    }

    if watch {
        let mut watcher = watch::FsWatcher::new(watch_options)?;
        // TODO(tailhook) do we have to monitor `{gel,edgedb}.toml` for the schema
        // dir change
        watcher.watch(&ctx.schema_dir, RecursiveMode::Recursive)?;

        let ok = matches!(single_check(ctx, cli).await?, Okay);
        if ok {
            print::success!("The schema is forward compatible. Ready for upgrade.");
        }
        eprintln!("Monitoring {:?} for changes.", &ctx.schema_dir);
        watch_loop(watcher, ctx, cli, ok).await?;
        Ok(())
    } else {
        match single_check(ctx, cli).await? {
            Okay => {}
            SchemaIssue => {
                msg!("For faster feedback loop use:");
                msg!(
                    "    {} {}",
                    BRANDING_CLI_CMD,
                    " migration upgrade-check --watch".emphasized()
                );
                return Err(ExitCode::new(3))?;
            }
            MigrationsIssue => {
                // Should be no need to watch
                return Err(ExitCode::new(4))?;
            }
        }
        if !ctx.quiet {
            msg!("The schema is forward compatible. Ready for upgrade.");
        }
        Ok(())
    }
}

async fn single_check(ctx: &Context, cli: &mut Connection) -> anyhow::Result<CheckResult> {
    use CheckResult::*;

    let bar = ProgressBar::new_spinner();
    bar.enable_steady_tick(Duration::from_millis(100));

    bar.set_message("checking schema");
    match execute_start_migration(ctx, cli).await {
        Ok(()) => {
            execute(cli, "ABORT MIGRATION", None).await?;
        }
        Err(e) if e.is::<SchemaFileError>() => {
            print::warn!(
                "Schema incompatibilities found. \
                  Please fix the errors above to proceed.",
            );
            return Ok(SchemaIssue);
        }
        Err(e) => return Err(e),
    }

    bar.set_message("checking migrations");
    let migrations = migration::read_all(ctx, true).await?;
    let old_timeout = timeout::inhibit_for_transaction(cli).await?;
    async_try! {
        async {
            execute(cli, "START MIGRATION REWRITE", None).await?;
            async_try! {
                async {
                    for migration in migrations.values() {
                        match apply_migration(cli, migration, false).await {
                            Ok(()) => {},
                            Err(e) if e.is::<ApplyMigrationError>() => {
                                bar.finish_and_clear();
                                print_apply_migration_error();
                                return Ok(MigrationsIssue);
                            }
                            Err(e) => return Err(e)?,
                        }
                    }
                    bar.finish_and_clear();
                    anyhow::Ok(Okay)
                },
                finally async {
                    execute_if_connected(cli, "ABORT MIGRATION REWRITE")
                        .await
                }
            }
        },
        finally async {
            timeout::restore_for_transaction(cli, old_timeout).await
        }
    }
}

fn print_apply_migration_error() {
    print::warn!(
        "The current schema is compatible, \
         but some of the migrations are outdated.",
    );
    msg!("Please squash all migrations to fix the issue:");
    msg!(
        "    {} {}",
        BRANDING_CLI_CMD,
        "migration create --squash".emphasized()
    );
}

pub async fn watch_loop(
    mut watcher: watch::FsWatcher,
    ctx: &Context,
    cli: &mut Connection,
    mut ok: bool,
) -> anyhow::Result<()> {
    let mut retry_timeout = None::<Duration>;
    loop {
        // note we don't wait for interrupt here because if interrupt happens
        // the `background_for` method of the process takes care of it.
        let event = cli.ping_while(watcher.wait(retry_timeout)).await;
        match event {
            watch::Event::Changed(_) | watch::Event::Retry => {}
            watch::Event::Abort => return Ok(()),
        };

        retry_timeout = None;
        match single_check(ctx, cli).await {
            Ok(CheckResult::Okay) => {
                if !ok {
                    print::success!(
                        "The schema is forward compatible. \
                            Ready for upgrade.",
                    );
                    ok = true;
                }
            }
            Ok(_) => {
                ok = false;
            }
            Err(e) => {
                ok = false;
                log::error!(
                    "Error updating database: {:#}. \
                             Will retry in 10s.",
                    e
                );
                retry_timeout = Some(Duration::from_secs(10));
            }
        }
    }
}
