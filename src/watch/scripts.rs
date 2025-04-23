use std::path;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::print;
use crate::process;

use super::Target;
use super::{Context, ExecutionOrder, Watcher};

pub async fn execute(
    mut input: UnboundedReceiver<ExecutionOrder>,
    matcher: Arc<Watcher>,
    ctx: Arc<Context>,
) {
    let project_root = &ctx.project.location.root;

    while let Some(order) = ExecutionOrder::recv(&mut input).await {
        order.print(&matcher, ctx.as_ref());

        let Target::Script(script) = &matcher.target else {
            unreachable!()
        };

        let res = run_script(matcher.name(), script, project_root).await;

        match res {
            Ok(status) => {
                if !status.success() {
                    print::error!("script exited with status {status}");
                }
            }
            Err(e) => {
                print::error!("{e}")
            }
        }
    }
}

pub async fn run_script(
    marker: &str,
    script: &str,
    current_dir: &path::Path,
) -> Result<std::process::ExitStatus, anyhow::Error> {
    let marker = marker.to_string();

    let status = if !cfg!(windows) {
        process::Native::new("", marker, "/bin/sh")
            .arg("-c")
            .arg(script)
            .current_dir(current_dir)
            .run_for_status()
            .await?
    } else {
        process::Native::new("", marker, "cmd.exe")
            .arg("/c")
            .arg(script)
            .current_dir(current_dir)
            .run_for_status()
            .await?
    };
    Ok(status)
}
