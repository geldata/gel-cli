use std::path::PathBuf;

use crate::branding::BRANDING_CLI_CMD;
use crate::commands::generate;
use crate::migrations::options::Migration;
use crate::options::{ConnectionOptions, InstanceOptions};
use crate::repl::{self, VectorLimit};
use crate::{branch, migrations};

use const_format::concatcp;

use gel_cli_derive::EdbSettings;

#[derive(clap::Subcommand, Clone, Debug)]
pub enum Common {
    /// Create database backup
    Dump(Dump),
    /// Restore database from backup file
    Restore(Restore),
    /// Modify database configuration
    Configure(crate::commands::configure::Command),

    /// Run language-specific code generators
    Generate(generate::Command),

    /// Migration management subcommands
    Migration(Box<Migration>),
    /// Apply migration (alias for [`BRANDING_CLI_CMD`] migration apply)
    Migrate(migrations::apply::Command),

    /// Database commands
    Database(Database),
    /// Manage branches
    Branch(branch::Command),
    /// Describe database schema or object
    Describe(Describe),

    /// List name and related info of database objects (types, scalars, modules, etc.)
    List(List),
    /// Analyze performance of query in quotes (e.g. `"select 9;"`)
    Analyze(Analyze),
    /// Show PostgreSQL address. Works on dev-mode database only.
    #[command(hide = true)]
    Pgaddr,
    /// Run psql shell. Works on dev-mode database only.
    #[command(hide = true)]
    Psql,
}

impl Common {
    pub fn as_migration(&self) -> Option<&Migration> {
        if let Common::Migration(m) = self {
            Some(m.as_ref())
        } else {
            None
        }
    }
}

#[derive(clap::Args, Clone, Debug)]
#[command(version = "help_expand")]
#[command(disable_version_flag = true)]
pub struct Describe {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    #[command(subcommand)]
    pub subcommand: DescribeCmd,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum DescribeCmd {
    /// Describe a database object
    Object(DescribeObject),
    /// Describe current database schema
    Schema(DescribeSchema),
}

#[derive(clap::Args, Clone, Debug)]
pub struct List {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    #[command(subcommand)]
    pub subcommand: ListCmd,
}

#[derive(clap::Args, Clone, Debug)]
pub struct Analyze {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    /// Query to analyze performance of
    pub query: Option<String>,

    /// Write analysis into specified JSON file instead of formatting
    #[arg(long)]
    pub debug_output_file: Option<PathBuf>,

    /// Read JSON file instead of executing a query
    #[arg(long, conflicts_with = "query")]
    pub read_json: Option<PathBuf>,

    /// Show detailed output of analyze command
    #[arg(long)]
    pub expand: bool,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum ListCmd {
    /// Display list of aliases defined in the schema
    Aliases(ListAliases),
    /// Display list of casts defined in the schema
    Casts(ListCasts),
    /// On EdgeDB < 5.x: Display list of databases for an instance
    Databases,
    /// On EdgeDB/Gel >= 5.x: Display list of branches for an instance
    Branches,
    /// Display list of indexes defined in the schema
    Indexes(ListIndexes),
    /// Display list of modules defined in the schema
    Modules(ListModules),
    /// Display list of roles for an instance
    Roles(ListRoles),
    /// Display list of scalar types defined in the schema
    Scalars(ListTypes),
    /// Display list of object types defined in the schema
    Types(ListTypes),
}

#[derive(clap::Args, Clone, Debug)]
#[command(version = "help_expand")]
#[command(disable_version_flag = true)]
pub struct Database {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    #[command(subcommand)]
    pub subcommand: DatabaseCmd,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum DatabaseCmd {
    /// Create a new database
    Create(CreateDatabase),
    /// Delete a database along with its data
    Drop(DropDatabase),
    /// Delete a database's data and reset its schema while preserving the
    /// database itself (its `cfg::DatabaseConfig`) and existing migration
    /// scripts
    Wipe(WipeDatabase),
}

#[derive(clap::Parser, Clone, Debug)]
#[command(no_binary_name = true, disable_help_subcommand(true))]
pub struct Backslash {
    #[command(subcommand)]
    pub command: BackslashCmd,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum BackslashCmd {
    #[command(flatten)]
    Common(Box<Common>),
    Help,
    LastError,
    Expand,
    DebugState(StateParam),
    DebugStateDesc(StateParam),
    History,
    Connect(Connect),
    Edit(Edit),
    Set(SetCommand),
    Exit,
}

#[derive(clap::Args, Clone, Debug)]
pub struct StateParam {
    /// Show base state (before transaction) instead of current transaction
    /// state
    ///
    /// Has no effect if currently not in a transaction
    #[arg(short = 'b')]
    pub base: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct SetCommand {
    #[command(subcommand)]
    pub setting: Option<Setting>,
}

#[derive(clap::Subcommand, Clone, Debug, EdbSettings)]
pub enum Setting {
    /// Query language. One of: edgeql, sql.
    Language(Language),
    /// Set input mode. One of: vi, emacs
    InputMode(InputMode),
    /// Print implicit properties of objects: id, type id
    ImplicitProperties(SettingBool),
    /// Print all errors with maximum verbosity
    VerboseErrors(SettingBool),
    /// Maximum number of items to display per query (default 100). Specify 0 to disable.
    Limit(Limit),
    /// Set maximum number of elements to display for ext::pgvector::vector type.
    ///
    /// Defaults to `auto` which displays whatever fits a single line, but no less
    /// than 3. Can be set to `unlimited` or a fixed number.
    VectorDisplayLength(VectorLimitValue),
    /// Set output format
    OutputFormat(OutputFormat),
    /// Set SQL output format
    SqlOutputFormat(OutputFormat),
    /// Display typenames in default output mode
    DisplayTypenames(SettingBool),
    /// Disable escaping newlines in quoted strings
    ExpandStrings(SettingBool),
    /// Set number of entries retained in history
    HistorySize(SettingUsize),
    /// Print statistics on each query
    PrintStats(PrintStats),
    /// Set idle transaction timeout in Duration format.
    /// Default is 5 minutes; specify 0 to disable.
    IdleTransactionTimeout(IdleTransactionTimeout),
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct Language {
    #[arg(value_name = "lang")]
    pub value: Option<repl::InputLanguage>,
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct InputMode {
    #[arg(value_name = "mode")]
    pub value: Option<repl::InputMode>,
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct SettingBool {
    #[arg(value_parser=["on", "off", "true", "false"])]
    pub value: Option<String>,
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct Limit {
    #[arg(value_name = "limit")]
    pub value: Option<usize>,
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct VectorLimitValue {
    #[arg(value_name = "limit")]
    pub value: Option<VectorLimit>,
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct IdleTransactionTimeout {
    #[arg(value_name = "duration")]
    pub value: Option<String>,
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct SettingUsize {
    pub value: Option<usize>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct Edit {
    #[arg(trailing_var_arg=true, allow_hyphen_values=true, num_args=..2)]
    pub entry: Option<isize>,
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct OutputFormat {
    #[arg(value_name = "mode")]
    pub value: Option<repl::OutputFormat>,
}

#[derive(clap::Args, Clone, Debug, Default)]
pub struct PrintStats {
    pub value: Option<repl::PrintStats>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct Connect {
    pub database_name: String,
}

#[derive(clap::Args, Clone, Debug)]
pub struct CreateDatabase {
    pub database_name: String,
}

#[derive(clap::Args, Clone, Debug)]
pub struct DropDatabase {
    pub database_name: String,
    /// Drop database without confirming
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct WipeDatabase {
    /// Drop database without confirming
    #[arg(long)]
    pub non_interactive: bool,

    #[command(flatten)]
    pub instance_opts: InstanceOptions,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ListAliases {
    pub pattern: Option<String>,
    #[arg(long, short = 'c')]
    pub case_sensitive: bool,
    #[arg(long, short = 's')]
    pub system: bool,
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ListCasts {
    pub pattern: Option<String>,
    #[arg(long, short = 'c')]
    pub case_sensitive: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ListIndexes {
    pub pattern: Option<String>,
    #[arg(long, short = 'c')]
    pub case_sensitive: bool,
    #[arg(long, short = 's')]
    pub system: bool,
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ListTypes {
    pub pattern: Option<String>,
    #[arg(long, short = 'c')]
    pub case_sensitive: bool,
    #[arg(long, short = 's')]
    pub system: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ListRoles {
    pub pattern: Option<String>,
    #[arg(long, short = 'c')]
    pub case_sensitive: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ListModules {
    pub pattern: Option<String>,
    #[arg(long, short = 'c')]
    pub case_sensitive: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct DescribeObject {
    pub name: String,
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

#[derive(clap::Args, Clone, Debug)]
pub struct DescribeSchema {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DumpFormat {
    Dir,
}

#[derive(clap::Args, Clone, Debug)]
pub struct Dump {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    /// Path to file write dump to (or directory if `--all` is specified).
    /// Use dash `-` to write to stdout (latter does not work in `--all` mode)
    #[arg(value_hint=clap::ValueHint::AnyPath)]
    pub path: PathBuf,
    /// Dump all databases and server configuration. `path` is a directory
    /// in this case and thus `--format=dir` is also required.  Will
    /// automatically overwrite any existing files of the same name.
    #[arg(long)]
    pub all: bool,

    /// Include secret configuration variables in the dump
    #[arg(long)]
    pub include_secrets: bool,

    /// Choose dump format. For normal dumps this parameter should be omitted.
    /// For `--all`, only `--format=dir` is required.
    #[arg(long, value_enum)]
    pub format: Option<DumpFormat>,

    /// Used to automatically overwrite existing files of the same name. Defaults
    /// to `true`.
    #[arg(long, default_value = "true")]
    pub overwrite_existing: bool,
}

#[derive(clap::Args, Clone, Debug)]
#[command(override_usage(concatcp!(
    BRANDING_CLI_CMD, " restore [OPTIONS] <path>\n    \
     Pre 5.0: ", BRANDING_CLI_CMD, " restore -d <database-name> <path>\n    \
     >=5.0:   ", BRANDING_CLI_CMD, " restore -b <branch-name> <path>"
)))]
pub struct Restore {
    #[command(flatten)]
    pub conn: Option<ConnectionOptions>,

    /// Path to file (or directory in case of `--all`) to read dump from.
    /// Use dash `-` to read from stdin
    #[arg(value_hint=clap::ValueHint::AnyPath)]
    pub path: PathBuf,

    /// Restore all databases and server configuration. `path` is a
    /// directory in this case
    #[arg(long)]
    pub all: bool,

    /// Verbose output
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

impl SettingBool {
    pub fn unwrap_value(&self) -> bool {
        match self.value.as_deref() {
            Some("on") => true,
            Some("off") => false,
            Some("true") => true,
            Some("false") => false,
            _ => unreachable!("validated by clap"),
        }
    }
}

impl std::str::FromStr for DumpFormat {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<DumpFormat, anyhow::Error> {
        match s {
            "dir" => Ok(DumpFormat::Dir),
            _ => Err(anyhow::anyhow!("unsupported dump format {:?}", s)),
        }
    }
}
