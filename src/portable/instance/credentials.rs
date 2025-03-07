use gel_dsn::gel::DEFAULT_PORT;
use gel_tokio::credentials::AsCredentials;
use std::io::{stdout, Write};
use url::Url;

use crate::options::{ConnectionOptions, Options};

pub fn show_credentials(options: &Options, c: &Command) -> anyhow::Result<()> {
    use gel_tokio::credentials::TlsSecurity;

    let connector = options.block_on_create_connector()?;
    let creds = connector.get()?;
    let creds = creds.as_credentials()?;
    if let Some(result) = if c.json {
        Some(serde_json::to_string_pretty(&creds)?)
    } else if c.insecure_dsn {
        let mut url = Url::parse(&format!(
            "edgedb://{}@{}:{}",
            creds.user,
            creds.host.unwrap_or("localhost".into()),
            creds.port.unwrap_or(DEFAULT_PORT),
        ))?;
        url.set_password(creds.password.as_deref()).ok();
        if let Some(database) = creds.database {
            url = url.join(&database)?;
        }
        match creds.tls_security {
            TlsSecurity::Strict => {
                url.set_query(Some("tls_security=strict"));
            }
            TlsSecurity::Insecure => {
                url.set_query(Some("tls_security=insecure"));
            }
            TlsSecurity::NoHostVerification => {
                url.set_query(Some("tls_security=no_host_verification"));
            }
            _ => {}
        }
        Some(url.to_string())
    } else {
        let mut settings = vec![
            ("Host", creds.host.unwrap_or("localhost".to_string())),
            ("Port", creds.port.unwrap_or(DEFAULT_PORT).to_string()),
            ("User", creds.user.clone()),
            (
                "Password",
                creds
                    .password
                    .as_ref()
                    .map(|_| "<hidden>".to_string())
                    .unwrap_or("<none>".to_string()),
            ),
            (
                "Database",
                creds.database.clone().unwrap_or("<default>".to_string()),
            ),
            ("TLS Security", format!("{:?}", creds.tls_security)),
        ];

        if let Some(server_name) = creds.tls_server_name {
            settings.push(("TLS Server Name", server_name.clone()));
        }

        crate::table::settings(&settings);
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
