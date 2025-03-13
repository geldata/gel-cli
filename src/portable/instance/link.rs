use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use gel_dsn::gel::DatabaseBranch;
use gel_tokio::builder::CertCheck;
use ring::digest;

use gel_errors::{
    ClientConnectionFailedError, ClientNoCredentialsError, Error, ErrorKind, PasswordRequired,
};
use gel_tokio::credentials::{AsCredentials, TlsSecurity};
use gel_tokio::{Builder, Config};
use rustyline::error::ReadlineError;

use crate::branding::{BRANDING_CLI_CMD, BRANDING_CLOUD};
use crate::connect::Connection;
use crate::credentials;
use crate::hint::HintExt;
use crate::options;
use crate::options::CloudOptions;
use crate::options::{ConnectionOptions, Options};
use crate::portable::options::InstanceName;
use crate::print::{self, Highlight};
use crate::question;
use crate::tty_password;

async fn ask_trust_cert(
    non_interactive: bool,
    trust_tls_cert: bool,
    quiet: bool,
    cert: Vec<u8>,
) -> Result<(), Error> {
    let fingerprint = digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, &cert);
    let (_, cert) = x509_parser::parse_x509_certificate(&cert).map_err(|e| {
        ClientConnectionFailedError::with_source(e).context("Failed to parse server certificate")
    })?;
    if trust_tls_cert {
        if !quiet {
            print::warn!("Trusting unknown server certificate: {fingerprint:?}");
        }
    } else if non_interactive {
        return Err(gel_errors::ClientConnectionFailedError::with_message(
            format!("Unknown server certificate: {fingerprint:?}",),
        ));
    } else {
        let mut q = question::Confirm::new(format!(
            "Unknown server certificate:\nFingerprint: {fingerprint:?}\nSubject: {}\nIssuer: {}\n\nTrust?",
            cert.subject(),
            cert.issuer(),
        ));
        q.default(false);
        if !q.async_ask().await? {
            return Err(gel_errors::ClientConnectionFailedError::with_message(
                format!("Unknown server certificate: {fingerprint:?}",),
            ));
        }
    }

    Ok(())
}

pub fn run(cmd: &Link, opts: &Options) -> anyhow::Result<()> {
    run_async(cmd, opts)
}

#[tokio::main(flavor = "current_thread")]
pub async fn run_async(cmd: &Link, opts: &Options) -> anyhow::Result<()> {
    if matches!(cmd.name, Some(InstanceName::Cloud { .. })) {
        anyhow::bail!(
            "{BRANDING_CLOUD} instances cannot be linked\
            \nTo connect run:\
            \n  {BRANDING_CLI_CMD} -I {}",
            cmd.name.as_ref().unwrap()
        );
    }

    let mut has_branch: bool = false;

    let mut builder = options::prepare_conn_params(opts)?;
    // If the user doesn't specify a TLS CA, we need to accept all certs
    if cmd.conn.tls_ca_file.is_none() {
        builder = builder.tls_security(TlsSecurity::Insecure);
    }
    let mut config = prompt_conn_params(&opts.conn_options, builder, cmd, &mut has_branch).await?;
    let mut creds = config.as_credentials()?;
    let cert_holder: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));

    // When linking to a new server, we may need to trust the TLS certificate
    let non_interactive = cmd.non_interactive;
    let trust_tls_cert = cmd.trust_tls_cert;
    let quiet = cmd.quiet;
    let mut connect_result = if cmd.conn.tls_ca_file.is_none() {
        gel_tokio::raw::Connection::connect_with_cert_check(
            &config,
            CertCheck::new_fn(move |cert| {
                ask_trust_cert(non_interactive, trust_tls_cert, quiet, cert.to_vec())
            }),
        )
        .await
    } else {
        gel_tokio::raw::Connection::connect(&config).await
    };

    let new_cert = if let Some(cert) = cert_holder.lock().unwrap().take() {
        let pem = pem::encode(&pem::Pem::new("CERTIFICATE", cert));
        Some(pem)
    } else {
        None
    };

    if let Err(e) = connect_result {
        eprintln!("Connection error: {e:?}");
        if e.is::<PasswordRequired>() {
            let password;

            if opts.conn_options.password_from_stdin {
                password = tty_password::read_stdin_async().await?
            } else if !cmd.non_interactive {
                password = tty_password::read_async(format!(
                    "Password for '{}': ",
                    config.user().escape_default()
                ))
                .await?;
            } else {
                return Err(e.into());
            }

            config = config.with_password(&password);
            creds.password = Some(password);
            #[allow(deprecated)]
            if let Some(pem) = &new_cert {
                config = config.with_pem_certificates(pem)?;
            }
            connect_result = Ok(gel_tokio::raw::Connection::connect(&config).await?);
        } else {
            return Err(e.into());
        }
    }
    let mut connection = Connection::from_raw(&config, connect_result.unwrap(), "link");
    let ver = connection.get_version().await?.clone();
    if !has_branch && opts.conn_options.branch.is_none() && opts.conn_options.database.is_none() {
        let branch = connection.get_current_branch().await?;

        if ver.specific().major >= 5 {
            config = config.with_db(DatabaseBranch::Branch(branch.to_string()));
            creds.branch = Some(branch.into());
            creds.database = None;
        } else {
            config = config.with_db(DatabaseBranch::Database(branch.to_string()));
            creds.database = Some(branch.to_string());
            creds.branch = None;
        }

        eprintln!("using the {}", config.db);
    }

    if let Some(pem) = &new_cert {
        creds.tls_ca = Some(pem.clone());
    }

    let (cred_path, instance_name) = match &cmd.name {
        Some(InstanceName::Local(name)) => (credentials::path(name)?, name.clone()),
        Some(InstanceName::Cloud { .. }) => unreachable!(),
        None => {
            let default = gen_default_instance_name(config.display_addr());
            if cmd.non_interactive {
                if !cmd.quiet {
                    eprintln!("Using generated instance name: {}", &default);
                }
                (credentials::path(&default)?, default)
            } else {
                loop {
                    let name =
                        question::String::new("Specify a new instance name for the remote server")
                            .default(&default)
                            .async_ask()
                            .await?;
                    if matches!(
                        InstanceName::from_str(&name),
                        Err(_) | Ok(InstanceName::Cloud { .. })
                    ) {
                        print::error!(
                            "Instance name must be a valid identifier, \
                             (regex: ^[a-zA-Z_0-9]+(-[a-zA-Z_0-9]+)*$)"
                        );
                        continue;
                    }
                    break (credentials::path(&name)?, name);
                }
            }
        }
    };
    if cred_path.exists() {
        if cmd.overwrite {
            if !cmd.quiet {
                print::warn!("Overwriting {}", cred_path.display());
            }
        } else if cmd.non_interactive {
            anyhow::bail!("File {} exists; aborting.", cred_path.display());
        } else {
            let mut q = question::Confirm::new_dangerous(format!(
                "{} already exists! Overwrite?",
                cred_path.display()
            ));
            q.default(false);
            if !q.async_ask().await? {
                anyhow::bail!("Canceled.")
            }
        }
    }

    credentials::write_async(&cred_path, &creds).await?;
    if !cmd.quiet {
        eprintln!(
            "{} To connect run:\
            \n  {BRANDING_CLI_CMD} -I {}",
            "Successfully linked to remote instance."
                .emphasized()
                .success(),
            instance_name.escape_default(),
        );
    }
    Ok(())
}

#[derive(clap::Args, Clone, Debug)]
pub struct Link {
    #[command(flatten)]
    pub conn: ConnectionOptions,

    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Specify a new instance name for the remote server. User will
    /// be prompted to provide a name if not specified.
    #[arg(value_hint=clap::ValueHint::Other)]
    pub name: Option<InstanceName>,

    /// Run in non-interactive mode (accepting all defaults).
    #[arg(long)]
    pub non_interactive: bool,

    /// Reduce command verbosity.
    #[arg(long)]
    pub quiet: bool,

    /// Trust peer certificate.
    #[arg(long)]
    pub trust_tls_cert: bool,

    /// Overwrite existing credential file if any.
    #[arg(long)]
    pub overwrite: bool,
}

fn gen_default_instance_name(input: impl fmt::Display) -> String {
    let input = input.to_string();
    let mut name = input
        .strip_suffix(":5656")
        .unwrap_or(&input)
        .chars()
        .map(|x| match x {
            'A'..='Z' => x,
            'a'..='z' => x,
            '0'..='9' => x,
            _ => '_',
        })
        .collect::<String>();
    if name.is_empty() {
        return "inst1".into();
    }
    if name.chars().next().unwrap().is_ascii_digit() {
        name.insert(0, '_');
    }
    name
}

async fn prompt_conn_params(
    options: &ConnectionOptions,
    mut builder: Builder,
    link: &Link,
    has_branch: &mut bool,
) -> anyhow::Result<Config> {
    if link.non_interactive && options.password {
        anyhow::bail!("--password and --non-interactive are mutually exclusive.")
    }

    if link.non_interactive {
        let config = match builder.clone().build() {
            Ok(config) => config,
            Err(e) if e.is::<ClientNoCredentialsError>() => {
                return Err(anyhow::anyhow!("no connection options are specified")).with_hint(
                    || {
                        format!(
                            "Remove `--non-interactive` option or specify \
                           `--host=localhost` and/or `--port=5656`. \
                           See `{BRANDING_CLI_CMD} --help-connect` for details",
                        )
                    },
                )?;
            }
            Err(e) => return Err(e)?,
        };
        if !link.quiet {
            eprintln!(
                "Authenticating to gel://{}@{}/{}",
                config.user(),
                config.display_addr(),
                config.db.name().unwrap_or_default(),
            );
        }
        Ok(config)
    } else if options.dsn.is_none() {
        let (config, _) = builder.clone().compute()?;

        if options.host.is_none() {
            let host = config
                .host
                .as_ref()
                .map(|h| h.to_string())
                .unwrap_or("localhost".to_string());
            builder = builder.host_string(
                &question::String::new("Specify server host")
                    .default(&host)
                    .async_ask()
                    .await?,
            );
        };
        if options.port.is_none() {
            let port = config.port.unwrap_or(5656).to_string();
            builder = builder.port(
                question::String::new("Specify server port")
                    .default(&port)
                    .async_ask()
                    .await?
                    .parse::<u16>()?,
            );
        }
        if options.user.is_none() {
            let user = config.user.as_deref().unwrap_or("edgedb");
            builder = builder.user(
                &question::String::new("Specify database user")
                    .default(user)
                    .async_ask()
                    .await?,
            );
        }

        if options.database.is_none() && options.branch.is_none() {
            loop {
                match question::String::new("Specify database/branch (CTRL + D for default)")
                    .async_ask()
                    .await
                {
                    Ok(s) => {
                        if s.is_empty() {
                            eprintln!("No database/branch specified!");
                            continue;
                        }
                        builder = builder.database(&s).branch(&s);
                        *has_branch = true;
                        break;
                    }
                    Err(e) => match e.downcast_ref() {
                        Some(ReadlineError::Eof) => {
                            break;
                        }
                        Some(_) | None => anyhow::bail!(e),
                    },
                };
            }
        }

        Ok(builder.build()?)
    } else {
        Ok(builder.build()?)
    }
}
