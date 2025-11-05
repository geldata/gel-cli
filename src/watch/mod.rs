mod fs_watcher;
mod migrate;
mod scripts;

pub use fs_watcher::{Event, FsWatcher, WatchOptions};
pub use scripts::run_script;
use wax::Pattern;

use std::collections::HashSet;
use std::iter::FromIterator;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;

#[allow(unused_imports)]
use crate::branding::{BRANDING_CLI_CMD, BRANDING_LOCAL_CONFIG_FILE, MANIFEST_FILE_DISPLAY_NAME};
use crate::hint::HintExt;
use crate::options::Options;
use crate::print::{self, AsRelativeToCurrentDir, Highlight};
use crate::project;

#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    /// Runs "`BRANDING_CLI_CMD` migration apply --dev-mode" on changes to schema definitions.
    ///
    /// This runs in addition to scripts in `MANIFEST_FILE_DISPLAY_NAME`.
    #[arg(short = 'm', long)]
    pub migrate: bool,

    /// Runs "`BRANDING_CLI_CMD` sync" on changes to gel.local.toml.
    ///
    /// This runs in addition to scripts in `MANIFEST_FILE_DISPLAY_NAME`.
    #[arg(short = 's', long)]
    pub sync: bool,

    #[arg(short = 'v', long)]
    pub verbose: bool,

    #[cfg(unix)]
    /// Do not exit when the parent process exits.
    #[arg(long)]
    pub no_exit_with_parent: bool,

    #[arg(long)]
    pub extend_gel_toml: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
pub async fn run(options: &Options, cmd: &Command) -> anyhow::Result<()> {
    // Lock the project and instance exclusively, and then drop the project lock
    // and leave the instance lock shared
    let mut project = project::ensure_ctx_async(None).await?;
    project.downgrade_instance_lock()?;
    project.drop_project_lock();
    if let Some(extend_gel_toml) = &cmd.extend_gel_toml {
        project = project.read_extended(extend_gel_toml)?
    }

    let ctx = Arc::new(Context {
        project,
        options: options.clone(),
        cmd: cmd.clone(),
    });

    // determine what we will be watching
    let matchers = assemble_matchers(cmd, &ctx.project)?;

    if cmd.migrate {
        print::msg!(
            "Hint: --migrate will apply any changes from your schema files to the database. \
            When ready to commit your changes, use:"
        );
        print::msg!(
            "1) `{BRANDING_CLI_CMD} migration create` to write those changes to a migration file,"
        );
        print::msg!(
            "2) `{BRANDING_CLI_CMD} migrate --dev-mode` to replace all synced \
            changes with the migration.\n"
        );
    }

    print::msg!(
        "{} {} for changes in:",
        "Monitoring".emphasized(),
        ctx.project.location.root.as_relative().display()
    );
    print::msg!("");
    for m in &matchers {
        print::msg!("  {}: {}", m.name, m.target.to_string().muted());
    }
    print::msg!("");

    // spawn tasks that will execute the scripts
    // these tasks wait for ExecutionOrders to be emitted into `tx`
    let (tx, join_handle) = start_executors(&matchers, &ctx).await?;

    #[allow(unused_mut)]
    let mut watch_options = WatchOptions::default();
    #[cfg(unix)]
    if !cmd.no_exit_with_parent {
        watch_options.exit_with_parent = true;
    }

    // watch file system, debounce and match to globs
    // sends events to executors via tx channel
    watch_and_match(&matchers, &tx, &ctx, watch_options).await?;

    // close all tx
    for t in tx {
        drop(t);
    }
    // wait for executors to finish
    join_handle.join_all().await;

    Ok(())
}

/// Information about the current watch process
struct Context {
    project: project::Context,
    options: Options,
    cmd: Command,
}

struct Matcher {
    name: String,
    globs: Vec<wax::Glob<'static>>,
    target: Target,
}

impl Matcher {
    fn name(&self) -> &str {
        self.name.as_str()
    }
}

enum Target {
    Script(String),
    MigrateDevMode,
    Sync,
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Script(s) => f.write_str(s),
            Target::MigrateDevMode => {
                f.write_str(BRANDING_CLI_CMD)?;
                f.write_str(" migration apply --dev-mode")
            }
            Target::Sync => {
                f.write_str(BRANDING_CLI_CMD)?;
                f.write_str(" configure apply")
            }
        }
    }
}

fn assemble_matchers(
    cmd: &Command,
    project: &project::Context,
) -> anyhow::Result<Vec<Arc<Matcher>>> {
    let watch_scripts = &project.manifest.watch;
    if watch_scripts.is_empty() && !cmd.migrate && !cmd.sync {
        return Err(
            anyhow::anyhow!("Missing [[watch]] entries in {MANIFEST_FILE_DISPLAY_NAME}")
                .with_hint(|| {
                    "For auto-apply migrations in dev mode (the old behavior \
                    of `edgedb watch`) use `--migrate` flag."
                        .to_string()
                })
                .into(),
        );
    }

    let mut matches: Vec<Arc<Matcher>> = Vec::new();
    for watch_script in watch_scripts {
        let mut watcher = Matcher {
            name: watch_script.files.join(","),
            globs: Vec::with_capacity(watch_script.files.len()),
            target: Target::Script(watch_script.script.clone()),
        };

        for glob in &watch_script.files {
            let glob = wax::Glob::new(glob)?.into_owned();

            watcher.globs.push(glob);
        }

        matches.push(Arc::new(watcher));
    }
    matches.sort_by(|a, b| b.name.cmp(&a.name));

    if cmd.migrate {
        let schema_dir = project.manifest.project().get_schema_dir();
        let schema_dir = schema_dir
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("bad path: {}", schema_dir.display()))?;
        let glob_str = format!("{schema_dir}/**/*.{{gel,esdl}}");
        let glob = wax::Glob::new(&glob_str)?.into_owned();

        matches.push(Arc::new(Matcher {
            name: "--migrate".into(),
            globs: vec![glob],
            target: Target::MigrateDevMode,
        }));
    }

    if cmd.sync {
        let glob = wax::Glob::new(BRANDING_LOCAL_CONFIG_FILE)?;
        matches.push(Arc::new(Matcher {
            name: BRANDING_LOCAL_CONFIG_FILE.into(),
            globs: vec![glob],
            target: Target::Sync,
        }));
    }

    Ok(matches)
}

#[derive(Clone)]
struct SyncTrigger {
    tx: UnboundedSender<ExecutionOrder>,
    pending: Arc<AtomicBool>,
}

impl SyncTrigger {
    fn new() -> (Self, UnboundedReceiver<ExecutionOrder>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let pending = Arc::new(AtomicBool::new(false));
        (SyncTrigger { tx, pending }, rx)
    }

    fn maybe_trigger(&self) {
        if self
            .pending
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.tx.send(ExecutionOrder::default()).ok();
        }
    }
}

async fn start_executors(
    matchers: &[Arc<Matcher>],
    ctx: &Arc<Context>,
) -> anyhow::Result<(Vec<UnboundedSender<ExecutionOrder>>, JoinSet<()>)> {
    let mut senders = Vec::with_capacity(matchers.len());
    let mut join_set = JoinSet::new();

    let (sync_trigger, mut sync_rx) = SyncTrigger::new();
    if let Some(matcher) = matchers.iter().find(|m| matches!(m.target, Target::Sync)) {
        let project = ctx.project.clone();
        let schema_dir = ctx.project.manifest.project().get_schema_dir();
        let matcher = matcher.clone();
        let ctx = ctx.clone();
        let pending_sync = sync_trigger.pending.clone();
        join_set.spawn(async move {
            loop {
                match project::config::apply(&project, true, false).await {
                    Ok(true) => {
                        print::success!("Configuration applied.");
                    }
                    Ok(false) => {
                        print::msg!("No configuration to apply.");
                    }
                    Err(err) => {
                        match project::sync::maybe_enable_missing_extension(err, &schema_dir) {
                            Ok(extension_name) => {
                                pending_sync.store(true, std::sync::atomic::Ordering::Relaxed);
                                print::warn!(
                                    "Failed to apply configuration due to missing extension \
                                        `{extension_name}`; it's now enabled."
                                );
                            }
                            Err(err) => print::error!("Failed to apply configuration: {err}"),
                        }
                    }
                }
                match ExecutionOrder::recv(&mut sync_rx).await {
                    Some(order) => order.print(&matcher, ctx.as_ref()),
                    None => break,
                }
            }
        });
    }

    for matcher in matchers {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        match &matcher.target {
            Target::Script(_) => {
                senders.push(tx);
                join_set.spawn(scripts::execute(rx, matcher.clone(), ctx.clone()));
            }
            Target::MigrateDevMode => {
                senders.push(tx);
                let migrator = migrate::Migrator::new(ctx.clone()).await?;
                join_set.spawn(migrator.run(rx, matcher.clone(), sync_trigger.clone()));
            }
            Target::Sync => {
                senders.push(sync_trigger.tx.clone());
            }
        }
    }
    Ok((senders, join_set))
}

async fn watch_and_match(
    matchers: &[Arc<Matcher>],
    tx: &[UnboundedSender<ExecutionOrder>],
    ctx: &Arc<Context>,
    watch_options: WatchOptions,
) -> anyhow::Result<()> {
    let project_root = &ctx.project.location.root;

    // collect all paths that need to be watched
    let mut paths_to_watch: Vec<PathBuf> = Vec::new();
    fn include_path(path: PathBuf, to_watch: &mut Vec<PathBuf>) {
        // skip this path if it is already covered
        let is_already_included = to_watch.iter().any(|p| path.starts_with(p));
        if is_already_included {
            return;
        }

        // remove all paths that will be covered by this path
        to_watch.retain(|p| !p.starts_with(&path));

        to_watch.push(path);
    }
    for matcher in matchers {
        for glob in &matcher.globs {
            let (invariant, _variant) = glob.clone().partition();
            include_path(invariant, &mut paths_to_watch);
        }
    }
    if ctx.cmd.migrate {
        include_path(
            ctx.project.manifest.project().get_schema_dir(),
            &mut paths_to_watch,
        );
    }

    // init watcher
    let mut watcher = fs_watcher::FsWatcher::new(watch_options)?;
    for path in paths_to_watch {
        watcher.watch(&path, notify::RecursiveMode::Recursive)?;
    }

    loop {
        // wait for changes
        let event = watcher.wait(None).await;

        let changed_paths = match event {
            Event::Changed(paths) => paths,
            Event::Retry => Default::default(),
            Event::Abort => break,
        };
        // strip prefix
        let changed_paths: Vec<_> = changed_paths
            .iter()
            .flat_map(|p| p.strip_prefix(&project_root).ok())
            .map(|p| (p, wax::CandidatePath::from(p)))
            .collect();

        // run all matching scripts
        for (watcher, tx) in std::iter::zip(matchers, tx) {
            // does it match?
            let matched_paths = changed_paths
                .iter()
                .filter(|x| watcher.globs.iter().any(|m| m.is_match(x.1.clone())))
                .map(|x| x.0.display().to_string())
                .collect::<Vec<_>>();
            if matched_paths.is_empty() {
                continue;
            }

            let order = ExecutionOrder {
                matched_paths: HashSet::from_iter(matched_paths),
            };
            tx.send(order).unwrap();
        }
    }
    Ok(())
}

#[derive(Default)]
struct ExecutionOrder {
    matched_paths: HashSet<String>,
}

impl ExecutionOrder {
    fn merge(&mut self, other: ExecutionOrder) {
        self.matched_paths.extend(other.matched_paths);
    }

    async fn recv(input: &mut UnboundedReceiver<ExecutionOrder>) -> Option<ExecutionOrder> {
        let mut order = input.recv().await?;
        loop {
            match input.try_recv() {
                Ok(o) => order.merge(o),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return None,
            }
        }
        Some(order)
    }

    fn print(&self, matcher: &Matcher, ctx: &Context) {
        // print
        print::msg!(
            "{}",
            format!(
                "--- {}: {} ---",
                matcher.name(),
                matcher.target.to_string().muted()
            )
        );
        if ctx.cmd.verbose {
            let mut matched_paths: Vec<_> = self.matched_paths.iter().map(|p| p.as_str()).collect();
            matched_paths.sort();
            let reason = matched_paths.join(", ");

            print::msg!("{}", format!("  triggered by: {reason}").muted());
        }
    }
}
