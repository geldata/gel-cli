use crate::portable::project;
use crate::print::{self, Highlight};

/// Runs project hooks of the given action.
/// Must not be called if --skip-hooks or GEL_SKIP_HOOKS is set.
#[tokio::main(flavor = "current_thread")]
pub async fn on_action_sync(
    action: &'static str,
    project: &project::Context,
) -> anyhow::Result<()> {
    on_action(action, project).await
}

/// Runs project hooks of the given action.
/// Must not be called if --skip-hooks or GEL_SKIP_HOOKS is set.
pub async fn on_action(action: &'static str, project: &project::Context) -> anyhow::Result<()> {
    let Some(script) = get_hook(action, &project.manifest) else {
        return Ok(());
    };

    print::msg!("{}", format!("hook {action}: {script}").muted());

    let status = crate::watch::run_script(action, script, &project.location.root).await?;

    // abort on error
    if !status.success() {
        return Err(anyhow::anyhow!(
            "Hook {action} exited with status {status}."
        ));
    }
    Ok(())
}

fn get_hook<'m>(
    action: &'static str,
    manifest: &'m project::manifest::Manifest,
) -> Option<&'m str> {
    let hooks = manifest.hooks.as_ref()?;
    let hook = match action {
        "project.init.before" => &hooks.project.as_ref()?.init.as_ref()?.before,
        "project.init.after" => &hooks.project.as_ref()?.init.as_ref()?.after,
        "branch.switch.before" => &hooks.branch.as_ref()?.switch.as_ref()?.before,
        "branch.switch.after" => &hooks.branch.as_ref()?.switch.as_ref()?.after,
        "branch.wipe.before" => &hooks.branch.as_ref()?.wipe.as_ref()?.before,
        "branch.wipe.after" => &hooks.branch.as_ref()?.wipe.as_ref()?.after,
        "migration.apply.before" => &hooks.migration.as_ref()?.apply.as_ref()?.before,
        "migration.apply.after" => &hooks.migration.as_ref()?.apply.as_ref()?.after,
        "schema.update.before" => &hooks.schema.as_ref()?.update.as_ref()?.before,
        "schema.update.after" => &hooks.schema.as_ref()?.update.as_ref()?.after,
        _ => panic!("unknown action"),
    };
    hook.as_deref()
}
