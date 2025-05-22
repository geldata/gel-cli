use std::sync::Arc;
use std::time::Duration;

use const_format::concatcp;

use edgeql_parser::helpers::quote_string;
use gel_tokio::Error;
use indicatif::ProgressBar;
use log::debug;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::branding::BRANDING_CLI_CMD;
use crate::connect::{Connection, Connector};
use crate::migrations::{self, dev_mode};
use crate::{git, msg, print};

use super::{Context, ExecutionOrder, Watcher};

pub struct Migrator {
    ctx: Arc<Context>,
    migration_ctx: migrations::Context,
    git_branch: Option<String>,
    connector: Connector,
    is_force_database_error: bool,
}

impl Migrator {
    pub async fn new(ctx: Arc<Context>) -> anyhow::Result<Self> {
        let git_branch = git::git_current_branch().await;

        let connector = ctx.options.create_connector().await?;
        Ok(Migrator {
            migration_ctx: migrations::Context::for_project(
                ctx.project.clone(),
                ctx.options.skip_hooks,
            )?,
            git_branch,
            ctx,
            connector,
            is_force_database_error: false,
        })
    }

    pub async fn run(
        mut self,
        mut input: UnboundedReceiver<ExecutionOrder>,
        matcher: Arc<Watcher>,
    ) {
        loop {
            if let Some(git_branch) = &self.git_branch {
                let mut first_detatched = true;
                debug!("Expecting git branch: {}", git_branch);
                loop {
                    let branch = git::git_current_branch().await;
                    debug!("Current git branch: {:?}", branch);
                    match branch {
                        Some(current_branch) if &current_branch != git_branch => {
                            print::error!(
                                "Current git branch ({current_branch}) is different from the branch used to create the database ({git_branch}), exiting watch mode"
                            );
                            std::process::exit(1);
                        }
                        Some(..) if !first_detatched => {
                            msg!("Repository is no longer detached, resuming watch mode");
                            break;
                        }
                        Some(..) => break,
                        None if first_detatched => {
                            msg!("Repository HEAD is detached, pausing watch mode");
                            first_detatched = false;
                            continue;
                        }
                        None => {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            }
            let res = self.migration_apply_dev_mode().await;

            if let Err(e) = &res {
                print::error!("{e}");
                // TODO
                // matcher.should_retry = true;
            }

            match ExecutionOrder::recv(&mut input).await {
                Some(order) => order.print(&matcher, self.ctx.as_ref()),
                None => break,
            }
        }

        self.cleanup().await;
    }

    async fn migration_apply_dev_mode(&mut self) -> anyhow::Result<()> {
        let bar = ProgressBar::new_spinner();
        bar.enable_steady_tick(Duration::from_millis(100));
        bar.set_message("Connecting");
        let mut cli = Box::pin(self.connector.connect()).await?;

        let old_state = cli.set_ignore_error_state();
        let result = dev_mode::migrate(&mut cli, &self.migration_ctx, &bar).await;
        cli.restore_state(old_state);

        bar.finish_and_clear();
        match result {
            Ok(()) => {
                if self.is_force_database_error {
                    clear_error(&mut cli).await;
                    self.is_force_database_error = false;
                }
            }
            Err(e) => {
                eprintln!("Schema migration error: {e:#}");
                set_error(&mut cli, e).await;
                // TODO(tailhook) probably only print if error doesn't match
                self.is_force_database_error = true;
            }
        }
        Ok(())
    }

    async fn cleanup(&mut self) {
        if !self.is_force_database_error {
            return;
        }
        let conn = Box::pin(self.connector.connect()).await;
        let mut conn = match conn {
            Ok(connection) => connection,
            Err(e) => {
                log::error!("Cannot clear error: {:#}", e);
                return;
            }
        };

        clear_error(&mut conn).await
    }
}

impl From<anyhow::Error> for ErrorJson {
    fn from(err: anyhow::Error) -> ErrorJson {
        if let Some(err) = err.downcast_ref::<Error>() {
            ErrorJson {
                kind: "WatchError",
                message: format!(
                    "error when trying to update the schema.\n  \
                    Original error: {}: {}",
                    err.kind_name(),
                    err.initial_message().unwrap_or(""),
                ),
                hint: Some(
                    concatcp!(
                        "see the window running \
                           `",
                        BRANDING_CLI_CMD,
                        " watch` for more info"
                    )
                    .into(),
                ),
                details: None,
                context: None, // TODO(tailhook)
            }
        } else {
            ErrorJson {
                kind: "WatchError",
                message: format!(
                    "error when trying to update the schema.\n  \
                    Original error: {err}"
                ),
                hint: Some(
                    concatcp!(
                        "see the window running \
                           `",
                        BRANDING_CLI_CMD,
                        " watch` for more info"
                    )
                    .into(),
                ),
                details: None,
                context: None,
            }
        }
    }
}

async fn clear_error(cli: &mut Connection) {
    let res = cli
        .execute("CONFIGURE CURRENT DATABASE RESET force_database_error", &())
        .await;
    let Err(e) = res else { return };
    log::error!("Cannot clear database error state: {:#}", e);
}

async fn set_error(cli: &mut Connection, e: anyhow::Error) {
    let data = serde_json::to_string(&ErrorJson::from(e)).unwrap();
    let res = cli
        .execute(
            &format!(
                "CONFIGURE CURRENT DATABASE SET force_database_error := {}",
                quote_string(&data)
            ),
            &(),
        )
        .await;
    let Err(e) = res else { return };
    log::error!("Cannot set database error state: {:#}", e);
}

#[derive(serde::Serialize)]
struct ErrorJson {
    #[serde(rename = "type")]
    kind: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<ErrorContext>,
}

#[derive(serde::Serialize)]
struct ErrorContext {
    line: u32,
    col: u32,
    start: usize,
    end: usize,
    filename: String,
}
