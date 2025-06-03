mod fs_watcher;
mod migrate;
mod scripts;

pub use fs_watcher::{Event, FsWatcher, WatchOptions};
pub use scripts::run_script;

use std::collections::HashSet;
use std::iter::FromIterator;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;

#[allow(unused_imports)]
use crate::branding::{BRANDING_CLI_CMD, MANIFEST_FILE_DISPLAY_NAME};
use crate::hint::HintExt;
use crate::options::Options;
use crate::portable::project;
use crate::print::{self, AsRelativeToCurrentDir, Highlight};

#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    /// Runs "{BRANDING_CLI_CMD} migration apply --dev-mode" on changes to schema definitions.
    ///
    /// This runs in addition to to scripts in {MANIFEST_FILE_DISPLAY_NAME}.
    #[arg(short = 'm', long)]
    pub migrate: bool,

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
    let matchers = assemble_watchers(cmd, &ctx.project)?;

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

struct Watcher {
    name: String,
    matchers: Vec<globset::GlobMatcher>,
    target: Target,
}

impl Watcher {
    fn name(&self) -> &str {
        self.name.as_str()
    }
}

enum Target {
    Script(String),
    MigrateDevMode,
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Script(s) => f.write_str(s),
            Target::MigrateDevMode => {
                f.write_str(BRANDING_CLI_CMD)?;
                f.write_str(" migration apply --dev-mode")
            }
        }
    }
}

fn assemble_watchers(
    cmd: &Command,
    project: &project::Context,
) -> anyhow::Result<Vec<Arc<Watcher>>> {
    let watch_scripts = &project.manifest.watch;
    if watch_scripts.is_empty() && !cmd.migrate {
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

    let mut watchers = Vec::new();
    for watch_script in watch_scripts {
        let mut watcher = Watcher {
            name: watch_script.files.join(","),
            matchers: Vec::with_capacity(watch_script.files.len()),
            target: Target::Script(watch_script.script.clone()),
        };

        for glob in &watch_script.files {
            let glob = globset::Glob::new(glob)?;

            watcher.matchers.push(glob.compile_matcher());
        }

        watchers.push(Arc::new(watcher));
    }
    watchers.sort_by(|a, b| b.name.cmp(&a.name));

    if cmd.migrate {
        let schema_dir = project.manifest.project().get_schema_dir();
        let schema_dir = schema_dir
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("bad path: {}", schema_dir.display()))?;
        let glob_str = format!("{schema_dir}/**/*.{{gel,esdl}}");
        let glob = globset::Glob::new(&glob_str)?;

        watchers.push(Arc::new(Watcher {
            name: "--migrate".into(),
            matchers: vec![glob.compile_matcher()],
            target: Target::MigrateDevMode,
        }));
    }

    Ok(watchers)
}

async fn start_executors(
    matchers: &[Arc<Watcher>],
    ctx: &Arc<Context>,
) -> anyhow::Result<(Vec<UnboundedSender<ExecutionOrder>>, JoinSet<()>)> {
    let mut senders = Vec::with_capacity(matchers.len());
    let mut join_set = JoinSet::new();
    for matcher in matchers {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        senders.push(tx);

        match &matcher.target {
            Target::Script(_) => join_set.spawn(scripts::execute(rx, matcher.clone(), ctx.clone())),
            Target::MigrateDevMode => {
                let migrator = migrate::Migrator::new(ctx.clone()).await?;

                join_set.spawn(migrator.run(rx, matcher.clone()))
            }
        };
    }
    Ok((senders, join_set))
}

async fn watch_and_match(
    watchers: &[Arc<Watcher>],
    tx: &[UnboundedSender<ExecutionOrder>],
    ctx: &Arc<Context>,
    watch_options: WatchOptions,
) -> anyhow::Result<()> {
    let mut watcher = fs_watcher::FsWatcher::new(watch_options)?;
    // TODO: watch only directories that are needed, not the whole project

    watcher.watch(&ctx.project.location.root, notify::RecursiveMode::Recursive)?;
    let schema_dir = ctx.project.manifest.project().get_schema_dir();
    if ctx.cmd.migrate && !schema_dir.starts_with(&ctx.project.location.root) {
        watcher.watch(
            &ctx.project.manifest.project().get_schema_dir(),
            notify::RecursiveMode::Recursive,
        )?;
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
            .flat_map(|p| p.strip_prefix(&ctx.project.location.root).ok())
            .map(|p| (p, globset::Candidate::new(p)))
            .collect();

        // run all matching scripts
        for (watcher, tx) in std::iter::zip(watchers, tx) {
            // does it match?
            let matched_paths = changed_paths
                .iter()
                .filter(|x| watcher.matchers.iter().any(|m| m.is_match_candidate(&x.1)))
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

    fn print(&self, matcher: &Watcher, ctx: &Context) {
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
