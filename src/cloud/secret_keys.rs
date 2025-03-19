use anyhow::Context;

use crate::branding::BRANDING_CLOUD;
use crate::cloud::client::CloudClient;
use crate::cloud::options;
use crate::cloud::options::SecretKeyCommand;
use crate::commands::ExitCode;
use crate::options::CloudOptions;

use crate::portable::exit_codes;

use crate::table::{self, Cell, Row, Table};

use crate::print::{self, Highlight, msg};
use crate::question;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct SecretKey {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub scopes: Vec<String>,

    #[serde(with = "humantime_serde")]
    pub created_on: std::time::SystemTime,

    #[serde(with = "humantime_serde")]
    pub expires_on: Option<std::time::SystemTime>,

    pub secret_key: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CreateSecretKeyInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub ttl: Option<String>,
}

pub fn main(cmd: &SecretKeyCommand, options: &CloudOptions) -> anyhow::Result<()> {
    use crate::cloud::options::SecretKeySubCommand::*;
    match &cmd.subcommand {
        List(c) => list(c, options),
        Create(c) => create(c, options),
        Revoke(c) => revoke(c, options),
    }
}

pub fn list(c: &options::ListSecretKeys, options: &CloudOptions) -> anyhow::Result<()> {
    do_list(c, &CloudClient::new(options)?)
}

#[tokio::main(flavor = "current_thread")]
pub async fn do_list(c: &options::ListSecretKeys, client: &CloudClient) -> anyhow::Result<()> {
    _do_list(c, client).await
}

pub async fn _do_list(c: &options::ListSecretKeys, client: &CloudClient) -> anyhow::Result<()> {
    let keys: Vec<SecretKey> = client.get("secretkeys/").await?;

    if c.json {
        println!("{}", serde_json::to_string_pretty(&keys)?);
    } else {
        print_table(keys.into_iter());
    }

    Ok(())
}

fn print_table(items: impl Iterator<Item = SecretKey>) {
    let mut table = Table::new();
    table.set_format(*table::FORMAT);
    table.set_titles(Row::new(
        ["ID", "Name", "Created", "Expires", "Scopes"]
            .iter()
            .map(|x| table::header_cell(x))
            .collect(),
    ));
    for key in items {
        table.add_row(Row::new(vec![
            Cell::new(&key.id),
            Cell::new(&key.name.unwrap_or_default()),
            Cell::new(&humantime::format_rfc3339_seconds(key.created_on).to_string()),
            Cell::new(
                &key.expires_on.map_or(String::from("does not expire"), |t| {
                    humantime::format_rfc3339_seconds(t).to_string()
                }),
            ),
            Cell::new(&key.scopes.join(", ")),
        ]));
    }
    if !table.is_empty() {
        table.printstd();
    } else {
        println!("No secret keys present.")
    }
}

pub fn create(c: &options::CreateSecretKey, options: &CloudOptions) -> anyhow::Result<()> {
    do_create(c, &CloudClient::new(options)?)
}

#[tokio::main(flavor = "current_thread")]
pub async fn do_create(c: &options::CreateSecretKey, client: &CloudClient) -> anyhow::Result<()> {
    _do_create(c, client).await
}

pub async fn _do_create(c: &options::CreateSecretKey, client: &CloudClient) -> anyhow::Result<()> {
    let mut params = CreateSecretKeyInput {
        name: c.name.clone(),
        description: c.description.clone(),
        scopes: c.scopes.clone(),
        ttl: c.expires.clone(),
    };

    if !c.non_interactive {
        if c.expires.is_none() {
            params.ttl = _ask_ttl()?;
        }
        if c.scopes.is_none() && !c.inherit_scopes {
            params.scopes = _ask_scopes()?;
        }
    }

    params.ttl = match params.ttl {
        None => None,
        Some(ref s) if s == "never" => None,
        Some(s) => Some(s),
    };

    let key: SecretKey = create_secret_key(client, &params).await?;

    if c.json {
        println!("{}", serde_json::to_string_pretty(&key)?);
    } else {
        let sk = key
            .secret_key
            .context("no valid secret key returned from server")?;
        if c.non_interactive {
            print!("{sk}");
        } else {
            msg!(
                "\nYour new {} {}",
                BRANDING_CLOUD,
                " secret key is printed below. \
                 Be sure to copy and store it securely, as you will \
                 not be able to see it again.\n"
                    .success()
            );
            msg!("{}", sk.emphasized());
        }
    }

    Ok(())
}

pub async fn create_secret_key(
    client: &CloudClient,
    params: &CreateSecretKeyInput,
) -> anyhow::Result<SecretKey> {
    client.post("secretkeys/", params).await
}

pub fn revoke(c: &options::RevokeSecretKey, options: &CloudOptions) -> anyhow::Result<()> {
    do_revoke(c, &CloudClient::new(options)?)
}

#[tokio::main(flavor = "current_thread")]
pub async fn do_revoke(c: &options::RevokeSecretKey, client: &CloudClient) -> anyhow::Result<()> {
    _do_revoke(c, client).await
}

pub async fn _do_revoke(c: &options::RevokeSecretKey, client: &CloudClient) -> anyhow::Result<()> {
    if !c.non_interactive {
        let q = question::Confirm::new_dangerous(format!(
            "Do you really want to revoke secret key {:?}?",
            c.secret_key_id
        ));
        if !q.ask()? {
            print::error!("Canceled.");
            return Err(ExitCode::new(exit_codes::NOT_CONFIRMED).into());
        }
    }

    let key: SecretKey = client
        .delete(format!("secretkeys/{}", c.secret_key_id))
        .await?;

    if c.json {
        println!("{}", serde_json::to_string_pretty(&key)?);
    } else {
        println!(
            "Secret key '{}' has been revoked and is no longer valid.",
            key.id
        );
    }

    Ok(())
}

fn _ask_ttl() -> anyhow::Result<Option<String>> {
    loop {
        let ttl = question::String::new(
            "\nPlease specify how long the secret key should \
            remain valid.\nUse duration units like `1h3m`, or `never` \
            if the key should never expire.",
        )
        .ask()?;

        let dur = match ttl.as_str() {
            "never" => Some(ttl),
            _ => match humantime::parse_duration(&ttl) {
                Ok(duration) => Some(humantime::format_duration(duration).to_string()),
                Err(e) => {
                    print::error!("{e}");
                    continue;
                }
            },
        };

        return Ok(dur);
    }
}

fn _ask_scopes() -> anyhow::Result<Option<Vec<String>>> {
    loop {
        let scopes = question::String::new(
            "\nPlease specify a whitespace-separated list of authorizations (scopes) \
            for the new secret key.\n\
            For example, to limit the access scope to a single database in a single instance:\n\n\
            \x20\x20instance:org/instance database:mydatabase roles.all\n\n\
            To inherit the scope of the current secret key, type `inherit`",
        )
        .ask()?;

        match scopes.as_str() {
            s if s.trim().is_empty() => {
                continue;
            }
            "inherit" => {
                return Ok(None);
            }
            _ => {
                return Ok(Some(
                    scopes.split_whitespace().map(|s| s.to_string()).collect(),
                ));
            }
        };
    }
}
