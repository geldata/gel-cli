use std::path::PathBuf;

use crate::branding::BRANDING_CLI_CMD;
use crate::migrations::options::Migration;
use crate::options::{ConnectionOptions, InstanceOptions};
use crate::repl::{self, VectorLimit};
use crate::{branch, migrations};

use const_format::concatcp;

use edgedb_cli_derive::EdbSettings;

#[derive(clap::Subcommand, Clone, Debug)]
pub enum Common {
    /// Create database backup
    Dump(Dump),
    /// Restore database from backup file
    Restore(Restore),
    /// Modify database configuration
    Configure(Configure),

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

#[derive(clap::Args, Clone, Debug)]
pub struct Configure {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    #[command(subcommand)]
    pub command: ConfigureCommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum ConfigureCommand {
    /// Insert another configuration entry to the list setting
    Insert(ConfigureInsert),
    /// Reset configuration entry (empty the list for list settings)
    Reset(ConfigureReset),
    /// Set scalar configuration value
    Set(ConfigureSet),
}

#[derive(clap::Args, Clone, Debug)]
pub struct ConfigureInsert {
    #[command(subcommand)]
    pub parameter: ListParameter,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ConfigureReset {
    #[command(subcommand)]
    pub parameter: ConfigParameter,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ConfigureSet {
    #[command(subcommand)]
    pub parameter: ValueParameter,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum ListParameter {
    /// Insert a client authentication rule
    #[command(name = "Auth")]
    Auth(AuthParameter),
}

#[derive(clap::Subcommand, Clone, Debug)]
#[command(rename_all = "snake_case")]
pub enum ValueParameter {
    /// Specifies the TCP/IP address(es) on which the server is to listen for
    /// connections from client applications.
    ///
    /// If the list is empty, the server will not listen on any IP interface
    /// whatsoever, in which case only Unix-domain sockets can be used to
    /// connect to it.
    ListenAddresses(ListenAddresses),

    /// The TCP port the server listens on; 5656 by default. Note that the
    /// same port number is used for all IP addresses the server listens on.
    ListenPort(ListenPort),

    /// The amount of memory the database uses for shared memory buffers.
    ///
    /// Corresponds to the PostgreSQL configuration parameter of the same
    /// name. Changing this value requires server restart.
    SharedBuffers(ConfigStr),

    /// The amount of memory used by internal query operations such as sorting.
    ///
    /// Corresponds to the PostgreSQL work_mem configuration parameter.
    QueryWorkMem(ConfigStr),

    /// The maximum amount of memory to be used by maintenance operations.
    ///
    /// Some of the operations that use this option are: vacuuming, link, index
    /// or constraint creation. A value without units is assumed to be
    /// kilobytes. Defaults to 64 megabytes (64MB).
    ///
    /// Corresponds to the PostgreSQL maintenance_work_mem configuration
    /// parameter.
    MaintenanceWorkMem(ConfigStr),

    /// Sets the planner’s assumption about the effective size of the disk
    /// cache available to a single query.
    ///
    /// Corresponds to the PostgreSQL configuration parameter of the same name.
    EffectiveCacheSize(ConfigStr),

    /// Sets the default data statistics target for the planner.
    ///
    /// Corresponds to the PostgreSQL configuration parameter of the same name.
    DefaultStatisticsTarget(ConfigStr),

    /// Controls the default isolation level of each new transaction,
    /// including implicit transactions. Defaults to `Serializable`.
    /// Note that changing this to a lower isolation level implies
    /// that the transactions are also read-only by default regardless
    /// of the value of the `default_transaction_access_mode` setting.
    DefaultTransactionIsolation(ConfigStr),

    /// Controls the default deferrable status of each new transaction.
    /// It currently has no effect on read-write transactions or those
    /// operating at isolation levels lower than `Serializable`.
    /// The default is `NotDeferrable`.
    DefaultTransactionDeferrable(ConfigStr),

    // Controls the default read-only status of each new transaction,
    // including implicit transactions. Defaults to `ReadWrite`.
    // Note that if `default_transaction_isolation` is set to any value
    // other than Serializable this parameter is implied to be
    // `ReadOnly` regardless of the actual value.
    DefaultTransactionAccessMode(ConfigStr),

    /// Sets the number of concurrent disk I/O operations that PostgreSQL
    /// expects can be executed simultaneously.
    ///
    /// Corresponds to the PostgreSQL configuration parameter of the same name.
    EffectiveIoConcurrency(ConfigStr),

    /// How long client connections can stay inactive before being closed by
    /// the server. Defaults to `60 seconds`; set to `0s` to disable.
    SessionIdleTimeout(ConfigStr),

    /// How long client connections can stay inactive while in a transaction.
    /// Defaults to 10 seconds; set to `0s` to disable.
    SessionIdleTransactionTimeout(ConfigStr),

    /// How long an individual query can run before being aborted. A value of
    /// `0s` disables the mechanism; it is disabled by default.
    QueryExecutionTimeout(ConfigStr),

    /// Defines whether to allow DDL commands outside of migrations.
    ///
    /// May be set to:
    /// * `AlwaysAllow`
    /// * `NeverAllow`
    AllowBareDdl(ConfigStr),

    /// Apply access policies
    ///
    /// User-specified access policies are not applied when set to `false`,
    /// allowing any queries to be executed.
    ApplyAccessPolicies(ConfigStr),

    /// Apply access policies in SQL queries.
    ///
    /// User-specified access policies are not applied when set to `false`,
    /// allowing any queries to be executed.
    ApplyAccessPoliciesPG(ConfigStr),

    /// Allow setting user-specified object identifiers.
    AllowUserSpecifiedId(ConfigStr),

    /// Web origins that are allowed to send HTTP requests to this server.
    CorsAllowOrigins(ConfigStrs),

    /// Recompile all cached queries on DDL if enabled.
    AutoRebuildQueryCache(ConfigStr),

    /// Timeout to recompile the cached queries on DDL.
    AutoRebuildQueryCacheTimeout(ConfigStr),

    /// When to store resulting SDL of a Migration. This may be slow.
    ///
    /// May be set to:
    /// * `AlwaysStore`
    /// * `NeverStore`
    StoreMigrationSdl(ConfigStr),

    /// The maximum number of concurrent HTTP connections.
    ///
    /// HTTP connections for the `std::net::http` module.
    HttpMaxConnections(ConfigStr),

    /// The name of the current email provider.
    CurrentEmailProviderName(ConfigStr),

    /// Whether to use the new simple scoping behavior (disable path factoring).
    SimpleScoping(ConfigStr),

    /// Whether to warn when depending on old scoping behavior.
    WarnOldScoping(ConfigStr),

    /// Select what queries are tracked in sys::QueryStats.
    TrackQueryStats(ConfigStr),
}

#[derive(clap::Subcommand, Clone, Debug)]
#[command(rename_all = "snake_case")]
pub enum ConfigParameter {
    /// Reset listen addresses to 127.0.0.1
    ListenAddresses,
    /// Reset port to 5656
    ListenPort,
    /// Clear authentication table (only admin socket can be used to connect)
    #[command(name = "Auth")]
    Auth,
    /// Reset shared_buffers PostgreSQL configuration parameter to default value
    SharedBuffers,
    /// Reset work_mem PostgreSQL configuration parameter to default value
    QueryWorkMem,
    /// Reset PostgreSQL configuration parameter of the same name
    MaintenanceWorkMem,
    /// Reset PostgreSQL configuration parameter of the same name
    EffectiveCacheSize,
    /// Reset PostgreSQL configuration parameter of the same name
    DefaultStatisticsTarget,
    /// Reset PostgreSQL configuration parameter of the same name
    DefaultTransactionIsolation,
    /// Reset PostgreSQL configuration parameter of the same name
    DefaultTransactionDeferrable,
    /// Reset PostgreSQL configuration parameter of the same name
    DefaultTransactionAccessMode,
    /// Reset PostgreSQL configuration parameter of the same name
    EffectiveIoConcurrency,
    /// Reset session idle timeout
    SessionIdleTimeout,
    /// Reset session idle transaction timeout
    SessionIdleTransactionTimeout,
    /// Reset query execution timeout
    QueryExecutionTimeout,
    /// Reset allow_bare_ddl parameter to `AlwaysAllow`
    AllowBareDdl,
    /// Reset apply_access_policies parameter to `true`
    ApplyAccessPolicies,
    /// Reset apply_access_policies_pg parameter to `false`
    ApplyAccessPoliciesPG,
    /// Reset allow_user_specified_id parameter to `false`
    AllowUserSpecifiedId,
    /// Reset cors_allow_origins to an empty set
    CorsAllowOrigins,
    /// Reset auto_rebuild_query_cache to `true`
    AutoRebuildQueryCache,
    /// Reset auto_rebuild_query_cache_timeout
    AutoRebuildQueryCacheTimeout,
    /// When to store resulting SDL of a Migration
    StoreMigrationSdl,
    /// The maximum number of concurrent HTTP connections.
    HttpMaxConnections,
    /// The name of the current email provider.
    CurrentEmailProviderName,
    /// Whether to use the new simple scoping behavior.
    SimpleScoping,
    /// Whether to warn when depending on old scoping behavior.
    WarnOldScoping,
    /// Select what queries are tracked in sys::QueryStats.
    TrackQueryStats,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ListenAddresses {
    pub address: Vec<String>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ListenPort {
    pub port: u16,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ConfigStr {
    pub value: String,
}

#[derive(clap::Args, Clone, Debug)]
pub struct ConfigStrs {
    pub values: Vec<String>,
}

#[derive(clap::Args, Clone, Debug)]
pub struct AuthParameter {
    /// Priority of the authentication rule. The lower the number, the
    /// higher the priority.
    #[arg(long)]
    pub priority: i64,

    /// The name(s) of the database role(s) this rule applies to. Will apply
    /// to all roles if set to '*'
    #[arg(long = "users")]
    pub users: Vec<String>,

    /// The name of the authentication method type. Valid values are: Trust
    /// for no authentication and SCRAM for SCRAM-SHA-256 password
    /// authentication.
    #[arg(long)]
    pub method: String,

    /// An optional comment for the authentication rule.
    #[arg(long)]
    pub comment: Option<String>,
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
