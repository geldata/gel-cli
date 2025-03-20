use std::path::PathBuf;

use clap::ValueHint;

#[cfg(doc)]
use crate::branding::BRANDING;
use crate::migrations;
use crate::options::ConnectionOptions;
use crate::portable::repository::Channel;
use crate::portable::ver;

use gel_cli_derive::IntoArgs;

#[derive(clap::Args, Clone, Debug)]
#[command(version = "help_expand")]
#[command(disable_version_flag = true)]
pub struct Migration {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    #[command(subcommand)]
    pub subcommand: MigrationCmd,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum MigrationCmd {
    /// Apply migration from latest migration script.
    Apply(Box<migrations::apply::Command>),
    /// Create migration script inside `/migrations`.
    Create(migrations::create::Command),
    /// Show current migration status.
    Status(ShowStatus),
    /// Show all migration versions.
    Log(MigrationLog),
    /// Edit migration file.
    ///
    /// Invokes $EDITOR on the last migration file, and then fixes
    /// migration id after editor exits. Defaults to vi (Notepad
    /// in Windows). Usually should be used for migrations that have
    /// not been applied yet.
    Edit(MigrationEdit),
    /// Check if current schema is compatible with new [`BRANDING`] version.
    UpgradeCheck(UpgradeCheck),
    /// Extract migration history from the database and write it to
    /// `<schema-dir>/migrations`.
    ///
    /// Useful when a direct DDL command has been used to change the schema and
    /// now [`BRANDING_CLI`] fails because the database migration history is
    /// ahead of the migration history inside `<schema-dir>/migrations`.
    Extract(ExtractMigrations),
    /// Upgrades the format of migration files.
    UpgradeFormat(MigrationUpgradeFormat),
}

#[derive(clap::Args, IntoArgs, Clone, Debug)]
pub struct MigrationConfig {
    /// Project schema directory.  The default is `dbschema/`,
    /// which can be changed by setting `project.schema-dir`
    /// in `{gel,edgedb}.toml`.
    #[arg(long, value_hint=ValueHint::DirPath)]
    pub schema_dir: Option<PathBuf>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ShowStatus {
    #[command(flatten)]
    pub cfg: MigrationConfig,

    /// Do not print any messages, only indicate success by exit status.
    #[arg(long)]
    pub quiet: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct MigrationLog {
    #[command(flatten)]
    pub cfg: MigrationConfig,

    /// Print revisions from the filesystem.
    /// (Database connection not required.)
    #[arg(long)]
    pub from_fs: bool,

    /// Print revisions from the database.
    /// (No filesystem schema is required.)
    #[arg(long)]
    pub from_db: bool,

    /// Sort migrations starting from newer to older, instead
    /// of the default older to newer.
    #[arg(long)]
    pub newest_first: bool,

    /// Show maximum N revisions (default: no limit).
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct MigrationEdit {
    #[command(flatten)]
    pub cfg: MigrationConfig,

    /// Do not check migration using the database connection.
    #[arg(long)]
    pub no_check: bool,
    /// Fix migration id non-interactively, and do not run editor.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(clap::Args, IntoArgs, Clone, Debug)]
pub struct UpgradeCheck {
    #[command(flatten)]
    pub cfg: MigrationConfig,

    /// Check upgrade to a specified version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_testing", "to_nightly", "to_channel",
    ])]
    pub to_version: Option<ver::Filter>,

    /// Check upgrade to latest nightly version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_testing", "to_channel",
    ])]
    pub to_nightly: bool,

    /// Check upgrade to latest testing version.
    #[arg(long)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_nightly", "to_channel",
    ])]
    pub to_testing: bool,

    /// Check upgrade to latest version in the channel.
    #[arg(long, value_enum)]
    #[arg(conflicts_with_all=&[
        "to_version", "to_nightly", "to_testing",
    ])]
    pub to_channel: Option<Channel>,

    /// Monitor schema changes and check again on change.
    #[arg(long)]
    pub watch: bool,

    #[arg(hide = true)]
    pub run_server_with_status: Option<PathBuf>,
}

#[derive(clap::Args, IntoArgs, Clone, Debug)]
pub struct ExtractMigrations {
    #[command(flatten)]
    pub cfg: MigrationConfig,
    /// Don't ask questions, only add missing files, abort if mismatching.
    #[arg(long)]
    pub non_interactive: bool,
    /// Force overwrite existing migration files.
    #[arg(long)]
    pub force: bool,
}

#[derive(clap::Args, IntoArgs, Clone, Debug)]
pub struct MigrationUpgradeFormat {
    #[command(flatten)]
    pub cfg: MigrationConfig,
}
