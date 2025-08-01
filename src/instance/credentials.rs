use gel_tokio::Config;
use gel_tokio::dsn::{Authentication, DatabaseBranch};
use std::io::{Write, stdout};

use crate::options::{ConnectionOptions, Options};

/// Create a table of human-readable credentials for the given config.
pub fn credentials_table(config: &Config) -> Vec<(&str, String)> {
    let mut credentials = vec![];

    let host = config
        .host
        .target_name()
        .expect("Failed to get host target name");
    if let Some(tcp) = host.tcp() {
        credentials.push(("Host", tcp.0.to_string()));
        credentials.push(("Port", tcp.1.to_string()));
    } else if let Some(path) = host.path() {
        credentials.push(("Path", path.to_string_lossy().to_string()));
    } else {
        credentials.push(("Target", "<unknown>".to_string()));
    }

    credentials.push(("User", config.user.to_string()));
    match &config.db {
        DatabaseBranch::Default => credentials.push(("Branch", "<default>".to_string())),
        DatabaseBranch::Branch(branch) => credentials.push(("Branch", branch.to_string())),
        DatabaseBranch::Database(db) => credentials.push(("Database", db.to_string())),
        DatabaseBranch::Ambiguous(db) => credentials.push(("Database", db.to_string())),
    }

    match config.authentication {
        Authentication::None => credentials.push(("Authentication", "<none>".to_string())),
        Authentication::Password(_) => credentials.push(("Password", "<hidden>".to_string())),
        Authentication::SecretKey(_) => credentials.push(("Secret Key", "<hidden>".to_string())),
    }

    credentials.push(("TLS Security", format!("{:?}", config.tls_security)));

    if config.tls_ca.is_some() {
        credentials.push(("TLS CA", "<specified>".to_string()));
    }
    if let Some(server_name) = &config.tls_server_name {
        credentials.push(("TLS Server Name", server_name.clone()));
    }

    credentials
}

pub fn show_credentials(options: &Options, c: &Command) -> anyhow::Result<()> {
    let connector = options.block_on_create_connector()?;
    let creds = connector.get()?;
    if let Some(result) = if c.json {
        let creds = creds.as_credentials()?;
        Some(serde_json::to_string_pretty(&creds)?)
    } else if c.insecure_dsn {
        creds.dsn_url()
    } else {
        crate::table::settings(&credentials_table(creds));
        None
    } {
        stdout()
            .lock()
            .write_all((result + "\n").as_bytes())
            .expect("stdout write succeeds");
    }
    Ok(())
}

#[derive(clap::Args, Clone, Debug)]
pub struct Command {
    #[command(flatten)]
    pub cloud_opts: ConnectionOptions,

    /// Output in JSON format (password is included in cleartext).
    #[arg(long)]
    pub json: bool,
    /// Output a DSN with password in cleartext.
    #[arg(long)]
    pub insecure_dsn: bool,
}
