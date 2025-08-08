#![allow(unused)]
use const_format::concatcp;

/// The product name.
pub const BRANDING: &str = if cfg!(feature = "gel") {
    "Gel"
} else {
    "EdgeDB"
};
/// The CLI name.
pub const BRANDING_CLI: &str = concatcp!(BRANDING, " CLI");
/// The cloud name.
pub const BRANDING_CLOUD: &str = concatcp!(BRANDING, " Cloud");

/// The CLI command name.
pub const BRANDING_CLI_CMD: &str = if cfg!(feature = "gel") {
    "gel"
} else {
    "edgedb"
};
/// The CLI command name for the alternative executable.
pub const BRANDING_CLI_CMD_ALT: &str = if cfg!(feature = "gel") {
    "edgedb"
} else {
    "gel"
};
/// The executable file name for the CLI.
pub const BRANDING_CLI_CMD_FILE: &str = if cfg!(windows) {
    concatcp!(BRANDING_CLI_CMD, ".exe")
} else {
    BRANDING_CLI_CMD
};
/// The executable file name for the CLI alternative.
pub const BRANDING_CLI_CMD_ALT_FILE: &str = if cfg!(windows) {
    concatcp!(BRANDING_CLI_CMD_ALT, ".exe")
} else {
    BRANDING_CLI_CMD_ALT
};

pub const BRANDING_SCHEMA_FILE_EXT_GEL: &str = "gel";
pub const BRANDING_SCHEMA_FILE_EXT_ESDL: &str = "esdl";
pub const BRANDING_SCHEMA_FILE_EXT: &str = if cfg!(feature = "gel") {
    BRANDING_SCHEMA_FILE_EXT_GEL
} else {
    BRANDING_SCHEMA_FILE_EXT_ESDL
};

/// The WSL distribution name.
pub const BRANDING_WSL: &str = "EdgeDB.WSL.1";

/// The display name for the project manifest file.
pub const MANIFEST_FILE_DISPLAY_NAME: &str = if cfg!(feature = "gel") {
    "`gel.toml` (or `edgedb.toml`)"
} else {
    "`edgedb.toml`"
};

/// The default query tag for server statistics.
pub const QUERY_TAG: &str = "gel/cli";
pub const REPL_QUERY_TAG: &str = "gel/repl";

/// The default name of the database user bootstrapped in a new instance.
pub const BRANDING_DEFAULT_USERNAME: &str = "admin";
pub const BRANDING_DEFAULT_USERNAME_LEGACY: &str = "edgedb";

/// Name of the server.
pub const BRANDING_SERVER: &str = if cfg!(feature = "gel") {
    "edgedb"
} else {
    "gel"
};

/// Name of the local configuration file.
pub const BRANDING_LOCAL_CONFIG_FILE: &str = "gel.local.toml";
