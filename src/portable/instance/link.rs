use std::fmt;
use std::sync::{Arc, Mutex};

use gel_tokio::builder::CertCheck;
use ring::digest;

use rustls::client::danger::HandshakeSignatureValid;
use rustls::client::danger::{ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use gel_errors::{ClientNoCredentialsError, Error, PasswordRequired};
use gel_tokio::credentials::TlsSecurity;
use gel_tokio::Client;
use gel_tokio::{Builder, Config};
use rustyline::error::ReadlineError;

use crate::branding::{BRANDING_CLI_CMD, BRANDING_CLOUD};
use crate::credentials;
use crate::hint::HintExt;
use crate::options;
use crate::options::CloudOptions;
use crate::options::{ConnectionOptions, Options};
use crate::portable::local::is_valid_local_instance_name;
use crate::portable::options::InstanceName;
use crate::portable::ver::Build;
use crate::print::{self, Highlight};
use crate::question;
use crate::tty_password;

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
    let mut config: Config = conn_params(cmd, opts, &mut has_branch).await?;
    let mut creds = config.as_credentials()?;
    // let root_cert_store = config.root_cert_store()?;
    // let inner = WebPkiServerVerifier::builder(Arc::new(root_cert_store)).build()?;
    // let verifier = Arc::new(InteractiveCertVerifier {
    //     inner,
    //     cert_out: Mutex::new(None),
    //     tls_security: creds.tls_security,
    //     system_ca_only: creds.tls_ca.is_none(),
    //     non_interactive: cmd.non_interactive,
    //     quiet: cmd.quiet,
    //     trust_tls_cert: cmd.trust_tls_cert,
    // });
    // let mut config = config.with_cert_verifier(verifier.clone());
    let mut connect_result = connect(&config).await;
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
            // if let Some(cert) = &*verifier.cert_out.lock().unwrap() {
            //     let pem = pem::encode(&pem::Pem::new("CERTIFICATE", cert.to_vec()));
            //     config = config.with_pem_certificates(&pem)?;
            // }
            connect_result = Ok(connect(&config).await?);
        } else {
        }
    }
    let mut connection: Client = connect_result.unwrap();
    let ver = get_server_version(&mut connection).await?;
    if !has_branch && opts.conn_options.branch.is_none() && opts.conn_options.database.is_none() {
        config = config.with_database(&get_current_branch(&mut connection).await?)?;

        eprintln!(
            "using the default {} '{}'",
            if ver.specific().major >= 5 {
                "branch"
            } else {
                "database"
            },
            config.database()
        );

        if ver.specific().major >= 5 {
            creds.branch = Some(config.database().to_string());
            creds.database = None;
        } else {
            creds.database = Some(config.database().to_string());
            creds.branch = None;
        }
    }

    // if let Some(cert) = &*verifier.cert_out.lock().unwrap() {
    //     creds.tls_ca = Some(pem::encode(&pem::Pem::new("CERTIFICATE", cert.to_vec())));
    // }

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
                    if !is_valid_local_instance_name(&name) {
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

#[derive(Debug)]
struct InteractiveCertVerifier {
    inner: Arc<WebPkiServerVerifier>,
    cert_out: Mutex<Option<Vec<u8>>>,
    tls_security: TlsSecurity,
    system_ca_only: bool,
    non_interactive: bool,
    quiet: bool,
    trust_tls_cert: bool,
}

impl ServerCertVerifier for InteractiveCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        use rustls::Error::InvalidCertificate;

        if let TlsSecurity::Insecure = self.tls_security {
            return Ok(ServerCertVerified::assertion());
        }
        match self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(val) => {
                return Ok(val);
            }
            Err(InvalidCertificate(cert_err))
                if matches!(cert_err, rustls::CertificateError::UnknownIssuer) =>
            {
                // reconstruct Error for easier fallthrough
                let e = InvalidCertificate(cert_err);

                if !self.system_ca_only {
                    // Don't continue if the verification failed when the user
                    // already specified a certificate to trust
                    return Err(e);
                }

                let mut root_store = rustls::RootCertStore::empty();
                root_store.add(end_entity.clone())?;
                // tls::NoHostnameVerifier::new(Arc::new(root_store)).verify_server_cert(
                //     end_entity,
                //     intermediates,
                //     server_name,
                //     ocsp_response,
                //     now,
                // )?;

                // Acquire consensus to trust the root of presented_certs chain
                let fingerprint = digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, end_entity);
                if self.trust_tls_cert {
                    if !self.quiet {
                        print::warn!("Trusting unknown server certificate: {fingerprint:?}");
                    }
                } else if self.non_interactive {
                    return Err(e);
                } else if let Ok(answer) = question::Confirm::new(format!(
                    "Unknown server certificate: {fingerprint:?}. Trust?",
                ))
                .default(false)
                .ask()
                {
                    if !answer {
                        return Err(e);
                    }
                } else {
                    return Err(e);
                }

                // Export the cert in PEM format and return verification success
                *self.cert_out.lock().unwrap() = Some(end_entity.to_vec());
            }
            Err(e) => return Err(e),
        }

        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
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

async fn connect(cfg: &gel_tokio::Config) -> Result<Client, Error> {
    //Connection::connect(cfg).await
    let client = gel_tokio::Client::new(cfg);
    client.ensure_connected().await?;

    Ok(client)
}

async fn conn_params(cmd: &Link, opts: &Options, has_branch: &mut bool) -> anyhow::Result<Config> {
    let mut builder = options::prepare_conn_params(opts)?;
    // If the user doesn't specify a TLS CA, we need to accept all certs
    if cmd.conn.tls_ca_file.is_none() {
        builder.tls_security(TlsSecurity::Insecure);
    }
    prompt_conn_params(&opts.conn_options, &mut builder, cmd, has_branch).await
}

async fn get_server_version(connection: &mut Client) -> anyhow::Result<Build> {
    let ver: String = connection
        .query_required_single("SELECT sys::get_version_as_str()", &())
        .await?;
    ver.parse()
}

async fn get_current_branch(connection: &mut Client) -> anyhow::Result<String> {
    let branch = connection
        .query_required_single::<String, _>("select sys::get_current_database()", &())
        .await;

    // for context why '?' isn't used, tokio swallows the error here and prints:
    // "edgedb error: ClientConnectionError: A Tokio 1.x context was found, but it is being shutdown."
    // whereas this ensures that the result is properly handled and the actual error is reported.
    if let Ok(branch) = branch {
        return Ok(branch);
    }

    anyhow::bail!(branch.unwrap_err());
}

async fn prompt_conn_params(
    options: &ConnectionOptions,
    builder: &mut Builder,
    link: &Link,
    has_branch: &mut bool,
) -> anyhow::Result<Config> {
    if link.non_interactive && options.password {
        anyhow::bail!("--password and --non-interactive are mutually exclusive.")
    }

    if link.non_interactive {
        let config = match builder.build_env().await {
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
                "Authenticating to edgedb://{}@{}/{}",
                config.user(),
                config.display_addr(),
                config.database(),
            );
        }
        Ok(config)
    } else if options.dsn.is_none() {
        let (_, config, _) = builder.build_no_fail().await;
        if options.host.is_none() {
            builder.host(
                &question::String::new("Specify server host")
                    .default(config.host().as_deref().unwrap_or("localhost"))
                    .ask()?,
            )?;
        };
        if options.port.is_none() {
            builder.port(
                question::String::new("Specify server port")
                    .default(&config.port().unwrap_or(5656).to_string())
                    .ask()?
                    .parse()?,
            )?;
        }
        if options.user.is_none() {
            builder.user(
                &question::String::new("Specify database user")
                    .default(config.user())
                    .ask()?,
            )?;
        }

        if options.database.is_none() && options.branch.is_none() {
            match question::String::new("Specify database/branch (CTRL + D for default)").ask() {
                Ok(s) => {
                    builder.database(&s)?.branch(&s)?;
                    *has_branch = true;
                }
                Err(e) => match e.downcast_ref() {
                    Some(ReadlineError::Eof) => {}
                    Some(_) | None => anyhow::bail!(e),
                },
            };
        }

        Ok(builder.build_env().await?)
    } else {
        Ok(builder.build_env().await?)
    }
}
