use std::collections::HashSet;
use std::path::{Path, PathBuf};

use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Duration;

const STABLE_TIME: Duration = Duration::from_millis(100);

#[derive(Debug, Default, Clone)]
pub struct WatchOptions {
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    pub exit_with_parent: bool,
}

pub struct FsWatcher {
    rx: mpsc::UnboundedReceiver<Vec<PathBuf>>,
    inner: notify::RecommendedWatcher,
    abort_rx: broadcast::Receiver<()>,
    abort_tasks: Vec<JoinHandle<()>>,
}

impl FsWatcher {
    #[cfg_attr(target_os = "windows", allow(unused_variables))]
    pub fn new(options: WatchOptions) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel::<Vec<PathBuf>>();
        let handler = WatchHandler { tx };
        let watch = notify::recommended_watcher(handler)?;

        let (abort_tx, abort_rx) = broadcast::channel(1);
        let mut abort_tasks = vec![];
        {
            let abort_tx = abort_tx.clone();
            abort_tasks.push(tokio::spawn(async move {
                let ctrl_c = crate::interrupt::Interrupt::ctrl_c();
                _ = ctrl_c.wait().await;
                _ = abort_tx.send(());
            }));
        }
        #[cfg(unix)]
        {
            let abort_tx = abort_tx.clone();
            abort_tasks.push(tokio::spawn(async move {
                use tokio::signal::unix::SignalKind;
                let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt()).unwrap();
                _ = sigint.recv().await;
                _ = abort_tx.send(());
            }));
        }
        #[cfg(unix)]
        if options.exit_with_parent {
            use std::os::unix::process::*;

            // On unix, detect parent death via reparenting.
            //
            // Why not PR_SET_PDEATHSIG? PR_SET_PDEATHSIG is not supported on
            // all unix systems, and regardless of support will fail
            // dramatically if the parent _thread_ dies. That is to say, if the
            // parent process uses a thread to spawn us, we'll die when the
            // thread dies rather than the process.
            let initial_pid = parent_id();
            let abort_tx = abort_tx.clone();
            abort_tasks.push(tokio::spawn(async move {
                while parent_id() == initial_pid {
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                }
                log::warn!(
                    "Parent process exited, exiting watch mode. Pass `--no-exit-with-parent` to prevent this.",
                );
                _ = abort_tx.send(());
            }));
        }

        Ok(FsWatcher {
            rx,
            inner: watch,
            abort_rx,
            abort_tasks,
        })
    }

    pub fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
        self.inner.watch(path, recursive_mode)
    }

    #[allow(dead_code)]
    pub fn clear_queue(&mut self) {
        while self.rx.try_recv().is_ok() {}
    }

    /// Waits for either changes in fs, timeout or interrupt signal
    pub async fn wait(&mut self, timeout: Option<Duration>) -> Event {
        tokio::select! {
            changes = Self::wait_for_changes(&mut self.rx) => Event::Changed(changes),
            _ = wait_for_timeout(timeout) => Event::Retry,
            _ = self.abort_rx.recv() => Event::Abort,
        }
    }

    /// Wait for changes in fs and debounce many consequent writes into a single event.
    async fn wait_for_changes(rx: &mut mpsc::UnboundedReceiver<Vec<PathBuf>>) -> HashSet<PathBuf> {
        let mut changed_paths = HashSet::new();

        let mut timeout = None;
        loop {
            tokio::select! {
                // when timeout runs out, return set of changed paths
                _ = wait_for_timeout(timeout) => { return changed_paths },

                // on new changed path
                paths = rx.recv() => {
                    // record the paths
                    if let Some(paths) = paths {
                        changed_paths.extend(paths);
                    } else {
                        return changed_paths;
                    }
                    // refresh the timeout
                    if changed_paths.is_empty() {
                        timeout = None;
                    } else {
                        timeout = Some(STABLE_TIME);
                    }
                },
            }
        }
    }
}

impl Drop for FsWatcher {
    fn drop(&mut self) {
        for task in self.abort_tasks.drain(..) {
            task.abort();
        }
    }
}

async fn wait_for_timeout(timeout: Option<Duration>) {
    tokio::time::sleep(timeout.unwrap_or(Duration::MAX)).await;
}

struct WatchHandler {
    tx: mpsc::UnboundedSender<Vec<PathBuf>>,
}

impl notify::EventHandler for WatchHandler {
    fn handle_event(&mut self, event: notify::Result<notify::Event>) {
        match event {
            Ok(e) => {
                if matches!(
                    e.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    let res = self.tx.send(e.paths);

                    if let Err(e) = res {
                        log::warn!("Error watching filesystem: {:#}", e)
                    }
                }
            }
            Err(e) => log::warn!("Error watching filesystem: {:#}", e),
        }
    }
}

#[derive(Debug)]
pub enum Event {
    /// Files have changed
    Changed(HashSet<PathBuf>),

    /// Timeout has been reached
    Retry,

    /// Abort watching
    Abort,
}
