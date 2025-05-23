use std::sync::Arc;
use std::time::Duration;

use const_format::concatcp;

use gel_tokio::Error;
use indicatif::ProgressBar;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::branding::BRANDING_CLI_CMD;
use crate::connect::Connector;
use crate::migrations::{self, dev_mode};
use crate::print;

use super::{Context, ExecutionOrder, Watcher};

pub struct Migrator {
    ctx: Arc<Context>,
    migration_ctx: migrations::Context,

    connector: Connector,
}

impl Migrator {
    pub async fn new(ctx: Arc<Context>) -> anyhow::Result<Self> {
        let connector = ctx.options.create_connector().await?;
        Ok(Migrator {
            migration_ctx: migrations::Context::for_project(
                ctx.project.clone(),
                ctx.options.skip_hooks,
            )?,
            ctx,
            connector,
        })
    }

    pub async fn run(
        mut self,
        mut input: UnboundedReceiver<ExecutionOrder>,
        matcher: Arc<Watcher>,
    ) {
        loop {
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
    }

    async fn migration_apply_dev_mode(&mut self) -> anyhow::Result<()> {
        let bar = ProgressBar::new_spinner();
        bar.enable_steady_tick(Duration::from_millis(100));
        bar.set_message("Connecting");
        let mut cli = Box::pin(self.connector.connect()).await?;

        let result = dev_mode::migrate(&mut cli, &self.migration_ctx, &bar).await;

        bar.finish_and_clear();
        if let Err(e) = result {
            eprintln!("Schema migration error: {e:#}");
        }
        Ok(())
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
