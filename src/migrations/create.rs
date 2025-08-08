use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::slice::Iter;
use std::sync::OnceLock;

use anyhow::Context as _;
use edgeql_parser::expr;
use edgeql_parser::hash::Hasher;
use edgeql_parser::schema_file::validate;
use edgeql_parser::tokenizer::{Kind as TokenKind, Tokenizer};
use fn_error_context::context;
use gel_derive::Queryable;
use gel_errors::{Error, InvalidSyntaxError, QueryError};
use immutable_chunkmap::set::SetM as Set;
use rustyline::error::ReadlineError;
use serde::Deserialize;
use tokio::fs;
use tokio::io::{self, AsyncWriteExt};
use tokio::task::spawn_blocking as unblock;

use crate::async_try;
use crate::branding::{BRANDING, BRANDING_CLI_CMD};
use crate::bug;
use crate::commands::{ExitCode, Options};
use crate::connect::Connection;
use crate::highlight;
use crate::migrations;
use crate::migrations::context::Context;
use crate::migrations::dev_mode;
use crate::migrations::edb::{execute, execute_if_connected, query_row};
use crate::migrations::migration;
use crate::migrations::print_error::print_migration_error;
use crate::migrations::prompt;
use crate::migrations::source_map::{Builder, SourceMap};
use crate::migrations::squash;
use crate::migrations::timeout;
use crate::platform::{is_legacy_schema_file, is_schema_file, tmp_file_name};
use crate::print::style::Styler;
use crate::print::{self, AsRelativeToCurrentDir, Highlight};
use crate::question;

const SAFE_CONFIDENCE: f64 = 0.99999;

pub async fn run(cmd: &Command, conn: &mut Connection, options: &Options) -> anyhow::Result<()> {
    if cmd.squash {
        squash::run(cmd, conn, options).await
    } else {
        run_inner(cmd, conn, options).await?;
        Ok(())
    }
}

#[derive(clap::Args, Clone, Debug)]
pub struct Command {
    #[command(flatten)]
    pub cfg: migrations::options::MigrationConfig,
    /// Squash all schema migrations into one and optionally provide a fixup migration.
    ///
    /// Note: this discards data migrations.
    #[arg(long)]
    pub squash: bool,
    /// Do not ask questions. By default works only if "safe" changes are
    /// to be done (those for which [`BRANDING`] has a high degree of confidence).
    /// This safe default can be overridden with `--allow-unsafe`.
    #[arg(long)]
    pub non_interactive: bool,
    /// Apply the most probable unsafe changes in case there are ones. This
    /// is only useful in non-interactive mode.
    #[arg(long)]
    pub allow_unsafe: bool,
    /// Create a new migration even if there are no changes (use this for
    /// data-only migrations).
    #[arg(long)]
    pub allow_empty: bool,
    /// Print queries executed.
    #[arg(long, hide = true)]
    pub debug_print_queries: bool,
    /// Show error details.
    #[arg(long, hide = true)]
    pub debug_print_err: bool,
    /// Do not print messages, only indicate success by exit status
    #[arg(long, hide = true)] // only used internally for now
    pub quiet: bool,
}

pub async fn run_inner(
    cmd: &Command,
    conn: &mut Connection,
    options: &Options,
) -> anyhow::Result<String> {
    let ctx = Context::for_migration_config(&cmd.cfg, false, options.skip_hooks, true).await?;

    if dev_mode::check_client(conn).await? {
        let dev_num = query_row::<i64>(
            conn,
            "SELECT count((
            SELECT schema::Migration
            FILTER .generated_by = schema::MigrationGeneratedBy.DevMode
        ))",
        )
        .await?;
        if dev_num > 0 {
            log::info!("Detected dev-mode migrations");
            return dev_mode::create(cmd, conn, options, &ctx).await;
        }
    }

    let migrations = migration::read_all(&ctx, true).await?;
    let old_timeout = timeout::inhibit_for_transaction(conn).await?;
    let migration = async_try! {
        async {
            // This decision must be done early on because
            // of the bug in EdgeDB:
            //   https://github.com/edgedb/edgedb/issues/3958
            if migrations.is_empty() {
                first_migration(conn, &ctx, cmd).await
            } else {
                let key = MigrationKey::Index((migrations.len() + 1) as u64);
                let parent = migrations.keys().last().map(|x| &x[..]);
                normal_migration(conn, &ctx, key, parent, cmd).await
            }
        },
        finally async {
            timeout::restore_for_transaction(conn, old_timeout).await
        }
    }?;
    write_migration(&ctx, &migration, !cmd.non_interactive).await
}

pub enum SourceName {
    Prefix,
    Semicolon(PathBuf),
    Suffix,
    File(PathBuf),
}

#[derive(Clone, Debug)]
enum Choice {
    Yes,
    No,
    List,
    Confirmed,
    Back,
    Split,
    Quit,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RequiredUserInput {
    placeholder: String,
    prompt: String,
    #[allow(dead_code)]
    old_type: Option<String>,
    old_type_is_object: Option<bool>,
    new_type: Option<String>,
    new_type_is_object: Option<bool>,
    #[serde(rename = "type")]
    type_name: Option<String>,
    pointer_name: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct StatementProposal {
    pub text: String,
}

#[derive(Deserialize, Debug)]
pub struct Proposal {
    pub prompt_id: Option<String>,
    pub statements: Vec<StatementProposal>,
    pub confidence: f64,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub required_user_input: Vec<RequiredUserInput>,
}

#[derive(Deserialize, Queryable, Debug)]
#[gel(json)]
pub struct CurrentMigration {
    pub complete: bool,
    pub parent: String,
    pub confirmed: Vec<String>,
    pub proposed: Option<Proposal>,
    pub debug_diff: Option<String>,
}

#[derive(Debug)]
pub enum MigrationKey {
    Index(u64),
    Fixup { target_revision: String },
}

pub trait MigrationToText<'a, T: Iterator<Item = &'a String> = std::iter::Once<&'a String>> {
    fn key(&self) -> &MigrationKey;
    fn parent(&self) -> anyhow::Result<&str>;
    fn id(&self) -> anyhow::Result<&str>;
    fn statements(&'a self) -> T;
}

#[derive(Debug)]
pub struct FutureMigration {
    key: MigrationKey,
    parent: String,
    statements: Vec<String>,
    id: OnceLock<anyhow::Result<String>>,
}

struct InteractiveMigration<'a> {
    cli: &'a mut Connection,
    save_point: usize,
    operations: Vec<Set<String>>,
    confirmed: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("refused to input data required for placeholder")]
struct Refused;

#[derive(Debug, thiserror::Error)]
#[error("split migration")]
struct SplitMigration;

#[derive(Debug, thiserror::Error)]
#[error(
    "{BRANDING} could not resolve migration automatically. \
         Please run `{BRANDING_CLI_CMD} migration create` in interactive mode."
)]
struct CantResolve;

#[derive(Debug, thiserror::Error)]
#[error("cannot proceed until schema files are fixed")]
pub struct SchemaFileError;

impl FutureMigration {
    fn new(key: MigrationKey, descr: CurrentMigration) -> Self {
        FutureMigration {
            key,
            parent: descr.parent,
            statements: descr.confirmed,
            id: OnceLock::new(),
        }
    }
    pub fn empty(key: MigrationKey, parent: &str) -> Self {
        FutureMigration {
            key,
            parent: parent.to_owned(),
            statements: Vec::new(),
            id: OnceLock::new(),
        }
    }
}

impl<'a> MigrationToText<'a, Iter<'a, String>> for FutureMigration {
    fn key(&self) -> &MigrationKey {
        &self.key
    }

    fn parent(&self) -> anyhow::Result<&str> {
        Ok(&self.parent)
    }

    fn id(&self) -> anyhow::Result<&str> {
        let FutureMigration {
            parent,
            statements,
            id,
            ..
        } = self;
        match id.get_or_init(|| {
            let mut hasher = Hasher::start_migration(parent);
            for statement in statements {
                hasher
                    .add_source(statement)
                    .map_err(|e| migration::hashing_error(statement, e))?;
            }
            Ok(hasher.make_migration_id())
        }) {
            Ok(s) => Ok(&s[..]),
            Err(e) => anyhow::bail!("Failed to generate migration id: {}", e),
        }
    }

    fn statements(&'a self) -> Iter<'a, String> {
        self.statements.iter()
    }
}

#[context("could not read schema file {}", path.display())]
async fn read_schema_file(path: &Path) -> anyhow::Result<String> {
    let data = fs::read_to_string(path).await?;
    validate(&data)?;
    Ok(data)
}

fn print_statements(statements: impl IntoIterator<Item = impl AsRef<str>>) {
    let mut buf: String = String::with_capacity(1024);
    let styler = Styler::new();
    for statement in statements {
        buf.truncate(0);
        highlight::edgeql(&mut buf, statement.as_ref(), &styler);
        for line in buf.lines() {
            println!("    {line}");
        }
    }
}

async fn choice(prompt: &str) -> anyhow::Result<Choice> {
    use Choice::*;

    let mut q = question::Choice::new(prompt.to_string());
    q.option(
        Yes,
        &["y", "yes"],
        r#"Confirm the prompt ("l" to see suggested statements)"#,
    );
    q.option(
        No,
        &["n", "no"],
        "Reject the prompt; server will attempt to generate another suggestion",
    );
    q.option(
        List,
        &["l", "list"],
        "List proposed DDL statements for the current prompt",
    );
    q.option(
        Confirmed,
        &["c", "confirmed"],
        "List already confirmed EdgeQL statements for the current migration",
    );
    q.option(
        Back,
        &["b", "back"],
        "Go back a step by reverting latest accepted statements",
    );
    q.option(
        Split,
        &["s", "stop"],
        "Stop and finalize migration with only current accepted changes",
    );
    q.option(Quit, &["q", "quit"], "Quit without saving changes");
    q.async_ask().await
}

#[context("could not read schema in {}", ctx.schema_dir.display())]
async fn gen_start_migration(ctx: &Context) -> anyhow::Result<(String, SourceMap<SourceName>)> {
    let mut bld = Builder::new();
    bld.add_lines(SourceName::Prefix, "START MIGRATION TO {");
    let mut dir = match fs::read_dir(&ctx.schema_dir).await {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            bld.add_lines(SourceName::Suffix, "};");
            return Ok(bld.done());
        }
        Err(e) => Err(e).context(format!("cannot read {:?}", ctx.schema_dir))?,
    };

    let mut paths: Vec<PathBuf> = Vec::new();
    let mut has_legacy_paths: bool = false;
    while let Some(item) = dir.next_entry().await? {
        let fname = item.file_name();
        let lossy_name = fname.to_string_lossy();
        if !lossy_name.starts_with('.')
            && is_schema_file(&lossy_name)
            && item.file_type().await?.is_file()
        {
            paths.push(item.path());
            if cfg!(feature = "gel") && is_legacy_schema_file(&lossy_name) {
                has_legacy_paths = true;
            }
        }
    }

    if cfg!(feature = "gel") && has_legacy_paths {
        print::warn!(
            "Legacy schema file extension '.esdl' detected. Consider renaming them to '.gel'."
        );
    }

    paths.sort();

    for path in paths {
        let chunk = read_schema_file(&path).await?;
        bld.add_lines(SourceName::File(path.clone()), &chunk);
        bld.add_lines(SourceName::Semicolon(path), ";");
    }

    bld.add_lines(SourceName::Suffix, "};");
    Ok(bld.done())
}

pub async fn execute_start_migration(ctx: &Context, cli: &mut Connection) -> anyhow::Result<()> {
    let (text, source_map) = gen_start_migration(ctx).await?;
    match execute(cli, text, Some(&source_map)).await {
        Ok(_) => Ok(()),
        Err(e) if e.is::<QueryError>() => {
            print_migration_error(&e, &source_map)?;
            Err(SchemaFileError)?
        }
        Err(e) => Err(e)?,
    }
}

pub async fn first_migration(
    cli: &mut Connection,
    ctx: &Context,
    options: &Command,
) -> anyhow::Result<FutureMigration> {
    execute_start_migration(ctx, cli).await?;
    async_try! {
        async {
            execute(cli, "POPULATE MIGRATION", None).await?;
            let descr = query_row::<CurrentMigration>(cli,
                "DESCRIBE CURRENT MIGRATION AS JSON"
            ).await?;
            if descr.parent != "initial" {
                // We know there are zero revisions in the filesystem
                anyhow::bail!("No database revision {} in \
                    the filesystem. Consider updating sources.",
                    descr.parent);
            }
            if !descr.complete {
                return Err(bug::error("First migration population is not complete"));
            }
            if descr.confirmed.is_empty() && !options.allow_empty {
                if !options.quiet {
                    print::warn!("No schema changes detected.");
                }
                return Err(ExitCode::new(4))?;
            }
            Ok(FutureMigration::new(MigrationKey::Index(1), descr))
        },
        finally async {
            execute_if_connected(cli, "ABORT MIGRATION").await
        }
    }
}

pub fn make_default_expression(input: &RequiredUserInput) -> Option<String> {
    let name = &input.placeholder[..];
    let kind_end = name.find("_expr").unwrap_or(name.len());
    let expr = match &name[..kind_end] {
        "fill" if input.type_name.is_some() => {
            format!("<{}>{{}}", input.type_name.as_ref().unwrap())
        }
        "cast" if input.pointer_name.is_some() && input.new_type.is_some() => {
            let pointer_name = input.pointer_name.as_deref().unwrap();
            let new_type = input.new_type.as_deref().unwrap();
            match (input.old_type_is_object, input.new_type_is_object) {
                (Some(true), Some(true)) => {
                    format!(".{pointer_name}[IS {new_type}]")
                }
                (Some(false), Some(false)) | (None, None) => {
                    format!("<{new_type}>.{pointer_name}")
                }
                // TODO(tailhook) maybe create something for mixed case?
                _ => return None,
            }
        }
        "conv" if input.pointer_name.is_some() => {
            format!("(SELECT .{} LIMIT 1)", input.pointer_name.as_ref().unwrap())
        }
        _ => {
            return None;
        }
    };
    Some(expr)
}

pub async fn unsafe_populate(
    _ctx: &Context,
    cli: &mut Connection,
) -> anyhow::Result<CurrentMigration> {
    loop {
        let data = query_row::<CurrentMigration>(cli, "DESCRIBE CURRENT MIGRATION AS JSON").await?;
        if data.complete {
            return Ok(data);
        }
        if let Some(proposal) = &data.proposed {
            let mut placeholders = BTreeMap::new();
            if !proposal.required_user_input.is_empty() {
                for input in &proposal.required_user_input {
                    let Some(expr) = make_default_expression(input) else {
                        log::debug!(
                            "Cannot fill placeholder {} \
                                    into {:?}, input info: {:?}",
                            input.placeholder,
                            proposal.statements,
                            input
                        );
                        return Err(CantResolve)?;
                    };
                    placeholders.insert(input.placeholder.clone(), expr);
                }
            }
            if !apply_proposal(cli, proposal, &placeholders).await? {
                execute(cli, "ALTER CURRENT MIGRATION REJECT PROPOSED", None).await?;
            }
        } else {
            log::debug!("No proposal generated");
            return Err(CantResolve)?;
        }
    }
}

async fn apply_proposal(
    cli: &mut Connection,
    proposal: &Proposal,
    placeholders: &BTreeMap<String, String>,
) -> anyhow::Result<bool> {
    execute(cli, "DECLARE SAVEPOINT proposal", None).await?;
    let mut rollback = false;
    async_try! {
        async {
            for statement in &proposal.statements {
                let statement = substitute_placeholders(
                    &statement.text, placeholders)?;
                if let Err(e) = execute(cli, &statement, None).await {
                    if e.is::<InvalidSyntaxError>() {
                        log::error!("Error executing: {statement}");
                        return Err(e)?;
                    } else if e.is::<QueryError>() {
                        rollback = true;
                        log::info!("Statement {statement:?} failed: {e:#}");
                        return Ok(false);
                    } else {
                        return Err(e)?;
                    }
                }
            }
            Ok(true)
        },
        finally async {
            if rollback {
                execute_if_connected(cli,
                    "ROLLBACK TO SAVEPOINT proposal",
                ).await?;
            }
            execute_if_connected(cli, "RELEASE SAVEPOINT proposal").await
        }
    }
}

async fn non_interactive_populate(
    _ctx: &Context,
    cli: &mut Connection,
) -> anyhow::Result<CurrentMigration> {
    loop {
        let data = query_row::<CurrentMigration>(cli, "DESCRIBE CURRENT MIGRATION AS JSON").await?;
        if data.complete {
            return Ok(data);
        }
        if let Some(proposal) = data.proposed {
            if proposal.confidence >= SAFE_CONFIDENCE {
                if !proposal.required_user_input.is_empty() {
                    for input in proposal.required_user_input {
                        eprintln!("Input required: {}", input.prompt);
                    }
                    anyhow::bail!("Cannot apply migration without user input");
                }
                for statement in proposal.statements {
                    execute(cli, &statement.text, None).await?;
                }
            } else {
                eprintln!("{BRANDING} intended to apply the following migration:");
                for statement in proposal.statements {
                    for line in statement.text.lines() {
                        eprintln!("    {line}");
                    }
                }
                eprintln!(
                    "But confidence is {}, below minimum threshold of {}",
                    proposal.confidence, SAFE_CONFIDENCE
                );
                anyhow::bail!(
                    "{BRANDING} is unable to make a decision. Please run in \
                    interactive mode to confirm changes, \
                    or use `--allow-unsafe`"
                );
            }
        } else {
            anyhow::bail!(
                "{BRANDING} could not resolve \
                migration automatically. Please run in \
                interactive mode to confirm changes, \
                or use `--allow-unsafe`"
            );
        }
    }
}

async fn run_non_interactive(
    ctx: &Context,
    cli: &mut Connection,
    key: MigrationKey,
    options: &Command,
) -> anyhow::Result<FutureMigration> {
    let descr = if options.allow_unsafe {
        unsafe_populate(ctx, cli).await?
    } else {
        non_interactive_populate(ctx, cli).await?
    };
    if descr.confirmed.is_empty() && !options.allow_empty {
        if !options.quiet {
            print::warn!("No schema changes detected.");
            //print::echo!("Hint: --allow-empty can be used to create a data-only migration with no schema changes.");
        }
        return Err(ExitCode::new(4))?;
    }
    Ok(FutureMigration::new(key, descr))
}

impl InteractiveMigration<'_> {
    fn new(cli: &mut Connection) -> InteractiveMigration {
        InteractiveMigration {
            cli,
            save_point: 0,
            operations: vec![Set::new()],
            confirmed: Vec::new(),
        }
    }
    async fn save_point(&mut self) -> Result<(), Error> {
        execute(
            self.cli,
            format!("DECLARE SAVEPOINT migration_{}", self.save_point),
            None,
        )
        .await
    }
    async fn rollback(&mut self) -> Result<(), Error> {
        execute(
            self.cli,
            format!("ROLLBACK TO SAVEPOINT migration_{}", self.save_point),
            None,
        )
        .await
    }
    async fn run(mut self, options: &Command) -> anyhow::Result<CurrentMigration> {
        self.save_point().await?;
        loop {
            let descr =
                query_row::<CurrentMigration>(self.cli, "DESCRIBE CURRENT MIGRATION AS JSON")
                    .await?;
            self.confirmed.clone_from(&descr.confirmed);
            if descr.complete {
                return Ok(descr);
            }
            if let Some(proposal) = &descr.proposed {
                match self.process_proposal(proposal).await {
                    Err(e) if e.is::<SplitMigration>() => return Ok(descr),
                    Err(e) => return Err(e),
                    Ok(()) => {}
                }
            } else {
                self.could_not_resolve(if options.debug_print_err {
                    descr.debug_diff
                } else {
                    None
                })
                .await?;
            }
        }
    }
    async fn process_proposal(&mut self, proposal: &Proposal) -> anyhow::Result<()> {
        use Choice::*;

        let cur_oper = self.operations.last().unwrap();
        let already_approved = proposal
            .prompt_id
            .as_ref()
            .map(|op| cur_oper.contains(op))
            .unwrap_or(false);
        let input;
        if already_approved {
            input = loop {
                println!("The following extra DDL statements will be applied:");
                for statement in &proposal.statements {
                    for line in statement.text.lines() {
                        println!("    {line}");
                    }
                }
                println!("(approved as part of an earlier prompt)");
                let input = self
                    .cli
                    .ping_while(get_user_input(&proposal.required_user_input))
                    .await;
                match input {
                    Ok(data) => break data,
                    Err(e) if e.is::<Refused>() => {
                        // TODO(tailhook) ask if we want to rollback or quit
                        continue;
                    }
                    Err(e) => return Err(e),
                };
            };
        } else {
            let prompt = if let Some(prompt) = &proposal.prompt {
                prompt
            } else {
                println!("The following DDL statements will be applied:");
                print_statements(proposal.statements.iter().map(|s| &s.text));
                "Apply the DDL statements?"
            };
            loop {
                match self.cli.ping_while(choice(prompt)).await? {
                    Yes => {
                        let input_res = self
                            .cli
                            .ping_while(get_user_input(&proposal.required_user_input))
                            .await;
                        match input_res {
                            Ok(data) => input = data,
                            Err(e) if e.is::<Refused>() => continue,
                            Err(e) => return Err(e),
                        };
                        break;
                    }
                    No => {
                        execute(self.cli, "ALTER CURRENT MIGRATION REJECT PROPOSED", None).await?;
                        self.save_point += 1;
                        self.save_point().await?;
                        return Ok(());
                    }
                    List => {
                        println!("The following DDL statements will be applied:");
                        print_statements(proposal.statements.iter().map(|s| &s.text));
                        continue;
                    }
                    Confirmed => {
                        if self.confirmed.is_empty() {
                            println!("No EdgeQL statements have been confirmed.");
                        } else {
                            println!("The following EdgeQL statements were confirmed:");
                            print_statements(&self.confirmed);
                        }
                        continue;
                    }
                    Back => {
                        if self.save_point == 0 {
                            eprintln!("No EdgeQL statements confirmed, nothing to move back from");
                            continue;
                        }
                        self.save_point -= 1;
                        self.rollback().await?;
                        self.operations.truncate(self.save_point + 1);
                        return Ok(());
                    }
                    Split => {
                        return Err(SplitMigration.into());
                    }
                    Quit => {
                        print::error!("Migration aborted; no results were saved.");
                        return Err(ExitCode::new(0))?;
                    }
                }
            }
        }
        for statement in &proposal.statements {
            let text = substitute_placeholders(&statement.text, &input)?;
            match execute(self.cli, &text, None).await {
                Ok(()) => {}
                Err(e) => {
                    if e.is::<QueryError>() {
                        print::query_error(&e, &text, false, "<statement>")?;
                    } else {
                        print::msg!(
                            "{}: {:#}",
                            "Error applying statement".emphasized().danger(),
                            e.to_string().emphasized(),
                        );
                    }
                    if self.cli.is_consistent() {
                        print::msg!("Rolling back last operation...");
                        self.rollback().await?;
                        return Ok(());
                    } else {
                        return Err(ExitCode::new(1).into());
                    }
                }
            }
        }
        if let Some(prompt_id) = &proposal.prompt_id {
            self.operations
                .push(self.operations.last().unwrap().insert(prompt_id.clone()).0);
        } else {
            self.operations
                .push(self.operations.last().unwrap().clone());
        }
        self.save_point += 1;
        self.save_point().await?;
        Ok(())
    }
    async fn could_not_resolve(&mut self, debug_info: Option<String>) -> anyhow::Result<()> {
        // TODO(tailhook) allow rollback
        let msg = match debug_info {
            Some(e) => format!(
                "{BRANDING} could not resolve migration with the provided answers. \
                Please retry with different answers.\n\n \
                Debug info:\n\n {e}"
            ),
            None => format!(
                "{BRANDING} could not resolve migration with the \
            provided answers. Please retry with different answers.",
            ),
        };

        anyhow::bail!("{}", msg)
    }
}

async fn run_interactive(
    _ctx: &Context,
    cli: &mut Connection,
    key: MigrationKey,
    options: &Command,
) -> anyhow::Result<FutureMigration> {
    let descr = InteractiveMigration::new(cli).run(options).await?;

    if descr.confirmed.is_empty() && !options.allow_empty {
        print::warn!("No schema changes detected.");
        //print::echo!("Hint: --allow-empty can be used to create a data-only migration with no schema changes.");
        return Err(ExitCode::new(4))?;
    }
    Ok(FutureMigration::new(key, descr))
}

pub async fn write_migration<'a, T>(
    ctx: &Context,
    descr: &'a impl MigrationToText<'a, T>,
    verbose: bool,
) -> anyhow::Result<String>
where
    T: Iterator<Item = &'a String>,
{
    let filename = match &descr.key() {
        MigrationKey::Index(idx) => {
            let dir = ctx.schema_dir.join("migrations");
            dir.join(format!("{:05}-{}.edgeql", idx, &descr.id()?[..7]))
        }
        MigrationKey::Fixup { target_revision } => {
            let dir = ctx.schema_dir.join("fixups");
            dir.join(format!("{}-{}.edgeql", descr.parent()?, target_revision))
        }
    };
    _write_migration(descr, filename.as_ref(), verbose).await
}

#[context("could not write migration file {}", filepath.display())]
async fn _write_migration<'a, T>(
    descr: &'a impl MigrationToText<'a, T>,
    filepath: &Path,
    verbose: bool,
) -> anyhow::Result<String>
where
    T: Iterator<Item = &'a String>,
{
    let id = descr.id()?;
    let dir = filepath.parent().unwrap();
    let tmp_file = filepath.with_file_name(tmp_file_name(filepath));
    if fs::metadata(filepath).await.is_err() {
        fs::create_dir_all(&dir).await?;
    }
    fs::remove_file(&tmp_file).await.ok();
    let mut file = io::BufWriter::new(fs::File::create(&tmp_file).await?);
    file.write_all(format!("CREATE MIGRATION {id}\n").as_bytes())
        .await?;
    file.write_all(format!("    ONTO {}\n", descr.parent()?).as_bytes())
        .await?;
    file.write_all(b"{\n").await?;
    for statement in descr.statements() {
        for line in statement.lines() {
            file.write_all(format!("  {line}\n").as_bytes()).await?;
        }
    }
    file.write_all(b"};\n").await?;
    file.flush().await?;
    drop(file);
    fs::rename(&tmp_file, &filepath).await?;

    let filepath = filepath.as_relative().display();
    if verbose {
        print::msg!(
            "{} {}, id: {id}",
            "Created".emphasized().success(),
            filepath.to_string().emphasized(),
        );
    }
    Ok(filepath.to_string())
}

pub async fn normal_migration(
    cli: &mut Connection,
    ctx: &Context,
    key: MigrationKey,
    ensure_parent: Option<&str>,
    create: &Command,
) -> anyhow::Result<FutureMigration> {
    execute_start_migration(ctx, cli).await?;
    async_try! {
        async {
            let descr = query_row::<CurrentMigration>(cli,
                "DESCRIBE CURRENT MIGRATION AS JSON",
            ).await?;
            let db_migration = if descr.parent == "initial" {
                None
            } else {
                Some(&descr.parent[..])
            };
            if db_migration != ensure_parent {
                anyhow::bail!("Database must be updated to the last migration \
                    on the filesystem for `migration create`. Run:\n  \
                    {BRANDING_CLI_CMD} migrate");
            }

            if create.non_interactive {
                run_non_interactive(ctx, cli, key, create).await
            } else {
                if create.allow_unsafe {
                    log::warn!("The `--allow-unsafe` flag is unused \
                                in interactive mode");
                }
                run_interactive(ctx, cli, key, create).await
            }
        },
        finally async {
            execute_if_connected(cli, "ABORT MIGRATION").await
        }
    }
}

fn add_newline_after_comment(value: &mut String) -> Result<(), anyhow::Error> {
    let last_token = Tokenizer::new(value)
        .last()
        .ok_or_else(|| bug::error("input should not be empty"))?
        .map_err(|e| bug::error(format!("tokenizer failed on reparsing: {e:#}")))?;
    let token_end = last_token.span.end as usize;
    if token_end < value.len() && !value[token_end..].trim().is_empty() {
        // Non-empty data after last token means comment.
        // Let's add a newline after input to make sure that
        // adding data after the input is safe
        value.push('\n');
    }
    Ok(())
}

fn get_input(req: &RequiredUserInput) -> Result<String, anyhow::Error> {
    let prompt = format!("{}> ", req.placeholder);
    let mut prev = make_default_expression(req).unwrap_or_default();
    loop {
        println!("{}:", req.prompt);
        let mut value = match prompt::expression(&prompt, &req.placeholder, &prev) {
            Ok(val) => val,
            Err(e) => match e.downcast::<ReadlineError>() {
                Ok(ReadlineError::Eof) => return Err(Refused.into()),
                Ok(e) => return Err(e.into()),
                Err(e) => return Err(e),
            },
        };
        match expr::check(&value) {
            Ok(()) => {}
            Err(e) => {
                println!("Invalid expression: {e}");
                prev = value;
                continue;
            }
        }
        add_newline_after_comment(&mut value)?;
        return Ok(value);
    }
}

async fn get_user_input(
    req: &[RequiredUserInput],
) -> Result<BTreeMap<String, String>, anyhow::Error> {
    let mut result = BTreeMap::new();
    for item in req {
        let copy = item.clone();
        let input = unblock(move || get_input(&copy)).await??;
        result.insert(item.placeholder.clone(), input);
    }
    Ok(result)
}

fn substitute_placeholders<'x>(
    input: &'x str,
    placeholders: &BTreeMap<String, String>,
) -> Result<Cow<'x, str>, anyhow::Error> {
    let mut output = String::with_capacity(input.len());
    let mut parser = Tokenizer::new(input);
    let mut start = 0;
    for item in &mut parser {
        let token = match item {
            Ok(item) => item,
            Err(e) => Err(bug::error(format!(
                "the server sent an invalid query: {e:#}"
            )))?,
        };
        if token.kind == TokenKind::Substitution {
            output.push_str(&input[start..token.span.start as usize]);
            let name = token
                .text
                .strip_prefix(r"\(")
                .and_then(|item| item.strip_suffix(')'))
                .ok_or_else(|| bug::error("bad substitution token".to_string()))?;
            let expr = placeholders
                .get(name)
                .ok_or_else(|| bug::error(format!("missing input for {name:?} placeholder")))?;
            output.push_str(expr);
            start = token.span.end as usize;
        }
    }
    if start == 0 {
        return Ok(input.into());
    }
    output.push_str(&input[start..]);
    Ok(output.into())
}

#[test]
fn placeholders() {
    let mut inputs = BTreeMap::new();
    inputs.insert("one".into(), " 1 ".into());
    inputs.insert("two".into(), "'two'".into());
    assert_eq!(
        substitute_placeholders(r"SELECT \(one);", &inputs).unwrap(),
        "SELECT  1 ;"
    );
    assert_eq!(
        substitute_placeholders(r"SELECT {\(one), \(two)};", &inputs).unwrap(),
        "SELECT { 1 , 'two'};"
    );
}

#[test]
fn add_newline() {
    fn wrapper(s: &str) -> String {
        let mut data = s.to_string();
        add_newline_after_comment(&mut data).unwrap();
        data
    }
    assert_eq!(wrapper("1+1"), "1+1");
    assert_eq!(wrapper("1    "), "1    ");
    assert_eq!(wrapper("1  #xx  "), "1  #xx  \n");
    assert_eq!(wrapper("(1 + 7) #xx"), "(1 + 7) #xx\n");
    assert_eq!(
        wrapper("(1 #one\n + 3 #three\n)"),
        "(1 #one\n + 3 #three\n)"
    );
}

#[tokio::test]
async fn start_migration() {
    use std::env;

    let mut schema_dir = env::current_dir().unwrap();
    schema_dir.push("tests/migrations/db5");

    let ctx = Context::for_temp_path(schema_dir).unwrap();

    let res = gen_start_migration(&ctx).await.unwrap();

    // Replace windows line endings \r\n with \n.
    let res_buf = res.0.replace("\r\n", "\n");

    let expected_buf =
        "START MIGRATION TO {\ntype Type1 {\n    property field1 -> str;\n};\n;\ntype Type2 {
    property field2 -> str;\n};\n;\ntype Type3 {\n    property field3 -> str;\n};\n;\n};\n";

    assert_eq!(res_buf, expected_buf);
}
