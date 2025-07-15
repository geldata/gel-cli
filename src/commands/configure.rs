use std::fmt::Display;

use crate::commands::Options;
use crate::connect::Connection;
use crate::options::ConnectionOptions;
use crate::print;
use edgeql_parser::helpers::{quote_name, quote_string};

pub async fn run(
    cmd: &Command,
    conn: &mut Connection,
    options: &Options,
) -> Result<(), anyhow::Error> {
    use ConfigureInsert as Ins;
    use ConfigureReset as Res;
    use ConfigureSet as Set;
    use ListParameter as I;
    use Subcommand as C;
    use ValueParameter as S;
    match &cmd.command {
        C::Apply(cmd) => crate::portable::project::config::run(cmd, options).await,

        C::Insert(Ins {
            parameter: I::Auth(param),
        }) => {
            let AuthParameter {
                users,
                comment,
                priority,
                method,
            } = param;
            let mut props = vec![
                format!("priority := {}", priority),
                format!("method := (INSERT {})", quote_name(method)),
            ];
            let users = users
                .iter()
                .map(|x| quote_string(x))
                .collect::<Vec<_>>()
                .join(", ");
            if !users.is_empty() {
                props.push(format!("user := {{ {users} }}"))
            }
            if let Some(comment_text) = comment {
                props.push(format!("comment := {}", quote_string(comment_text)))
            }
            let (status, _warnings) = conn
                .execute(
                    &format!(
                        r###"
                CONFIGURE INSTANCE INSERT Auth {{
                    {}
                }}
                "###,
                        props.join(",\n")
                    ),
                    &(),
                )
                .await?;
            print::completion(&status);
            Ok(())
        }
        C::Set(Set {
            parameter: S::ListenAddresses(ListenAddresses { address }),
        }) => {
            let addresses = address
                .iter()
                .map(|x| quote_string(x))
                .collect::<Vec<_>>()
                .join(", ");
            let (status, _warnings) = conn
                .execute(
                    &format!("CONFIGURE INSTANCE SET listen_addresses := {{{addresses}}}"),
                    &(),
                )
                .await?;
            print::completion(&status);
            Ok(())
        }
        C::Set(Set {
            parameter: S::ListenPort(param),
        }) => {
            let (status, _warnings) = conn
                .execute(
                    &format!("CONFIGURE INSTANCE SET listen_port := {}", param.port),
                    &(),
                )
                .await?;
            print::completion(&status);
            Ok(())
        }
        C::Set(Set {
            parameter: S::SharedBuffers(ConfigStr { value }),
        }) => set(conn, "shared_buffers", Some("<cfg::memory>"), value).await,
        C::Set(Set {
            parameter: S::QueryWorkMem(ConfigStr { value }),
        }) => set(conn, "query_work_mem", Some("<cfg::memory>"), value).await,
        C::Set(Set {
            parameter: S::MaintenanceWorkMem(ConfigStr { value }),
        }) => set(conn, "maintenance_work_mem", Some("<cfg::memory>"), value).await,
        C::Set(Set {
            parameter: S::EffectiveCacheSize(ConfigStr { value }),
        }) => set(conn, "effective_cache_size", Some("<cfg::memory>"), value).await,
        C::Set(Set {
            parameter: S::DefaultStatisticsTarget(ConfigStr { value }),
        }) => set(conn, "default_statistics_target", None, value).await,
        C::Set(Set {
            parameter: S::DefaultTransactionIsolation(ConfigStr { value }),
        }) => set(conn, "default_transcation_isolation", None, value).await,
        C::Set(Set {
            parameter: S::DefaultTransactionDeferrable(ConfigStr { value }),
        }) => set(conn, "default_transaction_deferrable", None, value).await,
        C::Set(Set {
            parameter: S::DefaultTransactionAccessMode(ConfigStr { value }),
        }) => set(conn, "default_transaction_access_mode", None, value).await,
        C::Set(Set {
            parameter: S::EffectiveIoConcurrency(ConfigStr { value }),
        }) => set(conn, "effective_io_concurrency", None, value).await,
        C::Set(Set {
            parameter: S::SessionIdleTimeout(ConfigStr { value }),
        }) => {
            set(
                conn,
                "session_idle_timeout",
                Some("<duration>"),
                format!("'{value}'"),
            )
            .await
        }
        C::Set(Set {
            parameter: S::SessionIdleTransactionTimeout(ConfigStr { value }),
        }) => {
            set(
                conn,
                "session_idle_transaction_timeout",
                Some("<duration>"),
                format!("'{value}'"),
            )
            .await
        }
        C::Set(Set {
            parameter: S::QueryExecutionTimeout(ConfigStr { value }),
        }) => {
            set(
                conn,
                "query_execution_timeout",
                Some("<duration>"),
                format!("'{value}'"),
            )
            .await
        }
        C::Set(Set {
            parameter: S::AllowBareDdl(ConfigStr { value }),
        }) => set(conn, "allow_bare_ddl", None, format!("'{value}'")).await,
        C::Set(Set {
            parameter: S::ApplyAccessPolicies(ConfigStr { value }),
        }) => set(conn, "apply_access_policies", None, value).await,
        C::Set(Set {
            parameter: S::ApplyAccessPoliciesPG(ConfigStr { value }),
        }) => set(conn, "apply_access_policies_pg", None, value).await,
        C::Set(Set {
            parameter: S::AllowUserSpecifiedId(ConfigStr { value }),
        }) => set(conn, "allow_user_specified_id", None, value).await,
        C::Set(Set {
            parameter: S::CorsAllowOrigins(ConfigStrs { values }),
        }) => {
            let values = values
                .iter()
                .map(|x| quote_string(x))
                .collect::<Vec<_>>()
                .join(", ");
            let (status, _warnings) = conn
                .execute(
                    &format!("CONFIGURE INSTANCE SET cors_allow_origins := {{{values}}}"),
                    &(),
                )
                .await?;
            print::completion(&status);
            Ok(())
        }
        C::Set(Set {
            parameter: S::AutoRebuildQueryCache(ConfigStr { value }),
        }) => set(conn, "auto_rebuild_query_cache", None, value).await,
        C::Set(Set {
            parameter: S::AutoRebuildQueryCacheTimeout(ConfigStr { value }),
        }) => {
            set(
                conn,
                "auto_rebuild_query_cache_timeout",
                Some("<duration>"),
                format!("'{value}'"),
            )
            .await
        }
        C::Set(Set {
            parameter: S::StoreMigrationSdl(ConfigStr { value }),
        }) => set(conn, "store_migration_sdl", None, format!("'{value}'")).await,
        C::Set(Set {
            parameter: S::HttpMaxConnections(ConfigStr { value }),
        }) => set(conn, "http_max_connections", None, value).await,
        C::Set(Set {
            parameter: S::CurrentEmailProviderName(ConfigStr { value }),
        }) => set(conn, "current_email_provider_name", None, value).await,
        C::Set(Set {
            parameter: S::SimpleScoping(ConfigStr { value }),
        }) => set(conn, "simple_scoping", None, value).await,
        C::Set(Set {
            parameter: S::WarnOldScoping(ConfigStr { value }),
        }) => set(conn, "warn_old_scoping", None, value).await,
        C::Set(Set {
            parameter: S::TrackQueryStats(ConfigStr { value }),
        }) => set(conn, "track_query_stats", None, value).await,
        C::Reset(Res { parameter }) => {
            use ConfigParameter as C;
            let name = match parameter {
                C::ListenAddresses => "listen_addresses",
                C::ListenPort => "listen_port",
                C::Auth => "Auth",
                C::SharedBuffers => "shared_buffers",
                C::QueryWorkMem => "query_work_mem",
                C::MaintenanceWorkMem => "maintenance_work_mem",
                C::EffectiveCacheSize => "effective_cache_size",
                C::DefaultStatisticsTarget => "default_statistics_target",
                C::DefaultTransactionAccessMode => "default_transaction_access_mode",
                C::DefaultTransactionDeferrable => "default_transaction_deferrable",
                C::DefaultTransactionIsolation => "default_transaction_isolation",
                C::EffectiveIoConcurrency => "effective_io_concurrency",
                C::SessionIdleTimeout => "session_idle_timeout",
                C::SessionIdleTransactionTimeout => "session_idle_transaction_timeout",
                C::QueryExecutionTimeout => "query_execution_timeout",
                C::AllowBareDdl => "allow_bare_ddl",
                C::ApplyAccessPolicies => "apply_access_policies",
                C::ApplyAccessPoliciesPG => "apply_access_policies_pg",
                C::AllowUserSpecifiedId => "allow_user_specified_id",
                C::CorsAllowOrigins => "cors_allow_origins",
                C::AutoRebuildQueryCache => "auto_rebuild_query_cache",
                C::AutoRebuildQueryCacheTimeout => "auto_rebuild_query_cache_timeout",
                C::StoreMigrationSdl => "store_migration_sdl",
                C::HttpMaxConnections => "http_max_connections",
                C::CurrentEmailProviderName => "current_email_provider_name",
                C::SimpleScoping => "simple_scoping",
                C::WarnOldScoping => "warn_old_scoping",
                C::TrackQueryStats => "track_query_stats",
            };
            let (status, _warnings) = conn
                .execute(&format!("CONFIGURE INSTANCE RESET {name}"), &())
                .await?;
            print::completion(&status);
            Ok(())
        }
    }
}

async fn set(
    cli: &mut Connection,
    name: &str,
    cast: Option<&str>,
    value: impl Display,
) -> Result<(), anyhow::Error> {
    let cast = cast.unwrap_or_default();
    let query = format!("CONFIGURE INSTANCE SET {name} := {cast}{value}");
    let (status, _warnings) = cli.execute(&query, &()).await?;
    print::completion(&status);
    Ok(())
}

#[derive(clap::Args, Clone, Debug)]
pub struct Command {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    #[command(subcommand)]
    pub command: Subcommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum Subcommand {
    /// Reads gel.local.toml from project directory and applies it to the instance.
    Apply(crate::portable::project::config::Command),
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
