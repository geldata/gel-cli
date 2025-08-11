use std::process::Command;

use gel_cli_instance::{ProcessError, ProcessErrorType, Processes, SystemProcessRunner};
use log::warn;

/// Get the current git branch.
///
/// Returns `None` if git is not installed. Returns `None` if the current branch
/// is not available, ie: if we are not in a git repository or the current HEAD
/// is detached.
pub async fn git_current_branch() -> Option<String> {
    let process_runner = SystemProcessRunner;

    let mut cmd = Command::new("git");
    cmd.args(["branch", "--show-current"]);
    match Processes::new(process_runner)
        .run_string(cmd)
        .await
        .map(|s| s.trim().to_string())
    {
        Ok(branch) if branch.is_empty() => None,
        Ok(branch) => Some(branch),
        Err(ProcessError {
            kind: ProcessErrorType::Io(e),
            ..
        }) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(ProcessError {
            kind: ProcessErrorType::CommandFailed(status, _),
            ..
        }) if status.code() == Some(128) => {
            // 128 = Running git command a non-git repo, silently return None
            None
        }
        Err(e) => {
            warn!("Failed to get current git branch: {e}");
            None
        }
    }
}
