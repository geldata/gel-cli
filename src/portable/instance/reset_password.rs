use gel_cli_derive::IntoArgs;
use gel_tokio::{InstanceName, dsn::DEFAULT_USER};
use rand::{Rng, SeedableRng};

use edgeql_parser::helpers::{quote_name, quote_string};

use crate::branding::{BRANDING_CLI_CMD, BRANDING_CLOUD, QUERY_TAG};
use crate::commands::ExitCode;
use crate::connect::Connection;
use crate::credentials;
use crate::options::InstanceOptionsLegacy;
use crate::portable::local::InstanceInfo;
use crate::print;
use crate::tty_password;

const PASSWORD_LENGTH: usize = 24;
const PASSWORD_CHARS: &[u8] = b"0123456789\
    abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const HASH_ITERATIONS: usize = 4096;
const SALT_LENGTH: usize = 16;

pub fn generate_password() -> String {
    let mut rng = rand::rngs::StdRng::from_entropy();
    (0..PASSWORD_LENGTH)
        .map(|_| PASSWORD_CHARS[rng.gen_range(0..PASSWORD_CHARS.len())] as char)
        .collect()
}

#[derive(clap::Args, IntoArgs, Debug, Clone)]
pub struct Command {
    #[command(flatten)]
    pub instance_opts: InstanceOptionsLegacy,

    /// User to change password for (default obtained from credentials file).
    #[arg(long)]
    pub user: Option<String>,
    /// Read password from the terminal rather than generating a new one.
    #[arg(long)]
    pub password: bool,
    /// Read password from stdin rather than generating a new one.
    #[arg(long)]
    pub password_from_stdin: bool,
    /// Save new user and password into a credentials file. By default
    /// credentials file is updated only if user name matches.
    #[arg(long)]
    pub save_credentials: bool,
    /// Do not save generated password into a credentials file even if user name matches.
    #[arg(long)]
    pub no_save_credentials: bool,
    /// Do not print any messages, only indicate success by exit status.
    #[arg(long)]
    pub quiet: bool,
}

pub fn run(options: &Command) -> anyhow::Result<()> {
    let instance = options.instance_opts.instance()?;
    let name = match &instance {
        InstanceName::Local(name) => {
            if cfg!(windows) {
                return crate::portable::windows::reset_password(options, &name);
            } else {
                name.clone()
            }
        }
        InstanceName::Cloud { .. } => {
            print::error!("This operation is not yet supported on {BRANDING_CLOUD} instances.");
            return Err(ExitCode::new(1))?;
        }
    };
    let creds = credentials::read(&instance)?;
    let (creds, save, user) = if let Some(creds) = creds {
        let user = options.user.clone().or(creds.user.clone());
        if options.no_save_credentials {
            (Some(creds), false, user)
        } else {
            let save = options.save_credentials || creds.user == user;
            (Some(creds), save, user)
        }
    } else {
        let user = options.user.clone();
        (None, !options.no_save_credentials, user)
    };
    let computed_user = user.clone().unwrap_or_else(|| DEFAULT_USER.into());
    let password = if options.password_from_stdin {
        tty_password::read_stdin()?
    } else if options.password {
        loop {
            let password = tty_password::read(format!(
                "New password for '{}': ",
                computed_user.escape_default()
            ))?;
            let confirm = tty_password::read(format!(
                "Confirm password for '{}': ",
                computed_user.escape_default()
            ))?;
            if password != confirm {
                print::error!("Passwords do not match");
            } else {
                break password;
            }
        }
    } else {
        generate_password()
    };

    let Ok(inst) = InstanceInfo::read(&name) else {
        anyhow::bail!(
            "Remote instances are not supported yet. Run `{BRANDING_CLI_CMD} -I {name}` in interactive mode and run `ALTER ROLE {computed_user} SET password := '...'`"
        );
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let config = inst.admin_conn_params()?;
            let mut cli = Connection::connect(&config, QUERY_TAG).await?;
            cli.execute(
                &format!(
                    r###"
                    ALTER ROLE {name} {{
                        SET password := {password};
                    }}"###,
                    name = quote_name(&computed_user),
                    password = quote_string(&password)
                ),
                &(),
            )
            .await?;
            Ok::<_, anyhow::Error>(())
        })?;

    if save {
        let mut creds = creds.unwrap_or_else(Default::default);
        creds.user = user;
        creds.password = Some(password);
        credentials::write(&instance, &creds)?;
    }
    if !options.quiet {
        if save {
            print::success!("Password was successfully changed and saved.",);
        } else {
            print::success!("Password was successfully changed.");
        }
    }
    Ok(())
}

pub fn password_hash(password: &str) -> String {
    let mut salt = [0u8; SALT_LENGTH];
    rand::thread_rng().fill(&mut salt);
    gel_auth::scram::StoredKey::generate(password.as_bytes(), &salt[..], HASH_ITERATIONS)
        .to_string()
}
