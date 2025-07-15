mod schema;
mod validation;

use edgeql_parser::helpers::quote_string as ql;
use gel_protocol::value::Value as GelValue;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::path;
use toml::Value as TomlValue;

use crate::branding::QUERY_TAG;
use crate::connect::Connection;
use crate::print;

#[tokio::main(flavor = "current_thread")]
pub async fn apply_local(project_root: &path::Path) -> anyhow::Result<()> {
    let local_toml = project_root.join("gel.local.toml");

    if !tokio::fs::try_exists(&local_toml).await? {
        print::msg!(
            "Writing gel.local.toml for config (it should be executed from source control)"
        );
        tokio::fs::write(&local_toml, INITIAL_CONFIG).await?;
        return Ok(());
    }

    // read toml
    let local_conf = tokio::fs::read_to_string(local_toml).await?;
    let toml = toml::de::Deserializer::new(&local_conf);
    let local_conf: LocalConfig = serde_path_to_error::deserialize(toml)?;
    let Some(config) = local_conf.local.and_then(|l| l.config) else {
        // there is no [config] table, don't sync
        return Ok(());
    };

    // validate
    let schema = schema::default_schema();
    let flat_config = validation::validate(config, &schema)?;
    if flat_config.is_empty() {
        return Ok(());
    }

    print::msg!("Applying config...");

    // configure
    let conn_config = gel_tokio::Builder::new()
        .with_fs()
        .with_explicit_project(project_root)
        .build()?;
    let mut conn = Connection::connect(&conn_config, QUERY_TAG).await?;
    conn.execute("START TRANSACTION;", &()).await?;
    configure(&mut conn, flat_config).await?;
    conn.execute("COMMIT;", &()).await?;

    print::msg!("Done.");

    Ok(())
}

async fn configure(
    conn: &mut Connection,
    flat_config: IndexMap<String, Value>,
) -> anyhow::Result<()> {
    for (name, value) in flat_config {
        match value {
            Value::Nested {
                values,
                is_root_config,
                ..
            } if is_root_config => {
                for (key, value) in values {
                    set_value(conn, value, &name, &key).await?;
                }
            }
            Value::Nested { .. } => {
                execute_configure(conn, &format!("reset {name}")).await?;
                insert_value(conn, value).await?;
            }
            Value::Set(values) => {
                execute_configure(conn, &format!("reset {name}")).await?;
                for value in values {
                    insert_value(conn, value).await?;
                }
            }
            _ => {
                panic!("expected object or set, got {value:?}");
            }
        }
    }
    Ok(())
}

async fn set_value(
    conn: &mut Connection,
    value: Value,
    config: &str,
    name: &str,
) -> anyhow::Result<()> {
    let config = match config {
        "cfg::Config" => "cfg",
        config => config,
    };
    let (value, args) = value.compile(0, 1);
    execute_configure_args(conn, &(format!("set {config}::{name} := {value}")), args).await
}

async fn insert_value(conn: &mut Connection, value: Value) -> anyhow::Result<()> {
    let Value::Nested {
        typ,
        values,
        is_root_config: false,
    } = value
    else {
        anyhow::bail!("Unsupported value type for inserting: {value:?}");
    };
    let mut args = HashMap::new();
    let values = values
        .into_iter()
        .map(|(name, val)| {
            let (val, value_args) = val.compile(args.len(), 2);
            args.extend(value_args);
            format!("{name} := {val}")
        })
        .collect::<Vec<_>>()
        .join(",\n\t");

    execute_configure_args(conn, &format!("insert {typ} {{\n\t{values}\n}}"), args).await
}

async fn execute_configure(conn: &mut Connection, query: &str) -> anyhow::Result<()> {
    let query = format!("configure current branch {query};");
    print::msg!("> {query}");
    conn.execute(&query, &()).await?;
    Ok(())
}

async fn execute_configure_args(
    conn: &mut Connection,
    query: &str,
    args: HashMap<String, gel_protocol::value::Value>,
) -> anyhow::Result<()> {
    let query = format!("configure current branch {query};");

    print::msg!("> {query}");
    if !args.is_empty() {
        print::msg!("\t with args: {args:?}");
    }

    let args: HashMap<&str, gel_protocol::value_opt::ValueOpt> = args
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone().into()))
        .collect();

    conn.execute(&query, &args).await?;
    Ok(())
}

/// Format of gel.local.toml
#[derive(Debug, serde::Deserialize)]
pub struct LocalConfig {
    local: Option<Local>,
}

#[derive(Debug, serde::Deserialize)]
pub struct Local {
    config: Option<TomlValue>,
}

#[derive(Debug)]
pub enum Value {
    Injected(String),
    Array(Vec<Value>),
    Set(Vec<Value>),
    Nested {
        typ: String,
        is_root_config: bool,
        values: IndexMap<String, Value>,
    },
}

impl TryFrom<TomlValue> for Value {
    type Error = anyhow::Error;

    fn try_from(value: TomlValue) -> anyhow::Result<Self> {
        Ok(match value {
            TomlValue::String(value) => {
                match value.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")) {
                    Some(value) => Value::Injected(value.to_string()),
                    None => Value::Injected(ql(&value)),
                }
            }
            TomlValue::Integer(value) => Value::Injected(value.to_string()),
            TomlValue::Float(value) => Value::Injected(value.to_string()),
            TomlValue::Boolean(value) => Value::Injected(value.to_string()),
            TomlValue::Datetime(
                datetimetz @ toml::value::Datetime {
                    date: Some(_),
                    time: Some(_),
                    offset: Some(_),
                },
            ) => Value::Injected(format!("<datetime>{}", datetimetz)),
            TomlValue::Datetime(
                datetime @ toml::value::Datetime {
                    date: Some(_),
                    time: Some(_),
                    offset: None,
                },
            ) => Value::Injected(format!("<cal::local_datetime>{}", datetime)),
            TomlValue::Datetime(toml::value::Datetime {
                date: Some(date),
                time: None,
                offset: None,
            }) => Value::Injected(format!("<cal::local_date>{}", date)),
            TomlValue::Datetime(toml::value::Datetime {
                date: None,
                time: Some(time),
                offset: None,
            }) => Value::Injected(format!("<cal::local_time>{}", time,)),
            TomlValue::Datetime(value) => {
                Err(anyhow::anyhow!("Invalid datetime value: {}", value))?
            }
            TomlValue::Array(values) => {
                let values = values
                    .into_iter()
                    .map(Self::try_from)
                    .collect::<anyhow::Result<Vec<_>>>()?;
                if values.iter().any(Value::is_object) {
                    Value::Set(values)
                } else {
                    Value::Array(values)
                }
            }
            TomlValue::Table(mut table) => {
                let Some(TomlValue::String(typ)) = table.remove("_tname") else {
                    anyhow::bail!("missing tname");
                };
                let values = table
                    .into_iter()
                    .map(|(k, v)| Self::try_from(v).map(|v| (k, v)))
                    .collect::<anyhow::Result<_>>()?;
                Self::Nested {
                    typ,
                    values,
                    is_root_config: false,
                }
            }
        })
    }
}

impl Value {
    pub fn is_object(&self) -> bool {
        match self {
            Value::Set(values) => values.iter().any(Self::is_object),
            Value::Nested { .. } => true,
            _ => false,
        }
    }

    fn compile(self, arg_index: usize, indent: i32) -> (String, HashMap<String, GelValue>) {
        let padding = "\t".repeat(indent as usize);
        let padding_closing = "\t".repeat(indent as usize - 1);
        match self {
            Value::Injected(value) => (value, HashMap::new()),
            Value::Set(values) => {
                let mut args = HashMap::new();
                let values = values
                    .into_iter()
                    .map(|value| {
                        let (value, value_args) = value.compile(arg_index + args.len(), indent + 1);
                        args.extend(value_args);
                        value
                    })
                    .collect::<Vec<_>>()
                    .join(&format!(",\n{padding}"));
                (format!("{{\n{padding}{values}\n{padding_closing}}}"), args)
            }
            Value::Array(values) => {
                let mut args = HashMap::new();
                let values = values
                    .into_iter()
                    .map(|v| {
                        let (value, value_args) = v.compile(args.len(), indent + 1);
                        args.extend(value_args);
                        value
                    })
                    .collect::<Vec<_>>()
                    .join(&format!(",\n{padding}"));

                (format!("[\n{padding}{values}\n{padding_closing}]"), args)
            }
            Value::Nested { typ, values, .. } => {
                let mut args = HashMap::new();
                let values = values
                    .into_iter()
                    .map(|(name, value)| {
                        let (value, value_args) = value.compile(arg_index + args.len(), indent + 1);
                        args.extend(value_args);
                        format!("{name} := {value}")
                    })
                    .collect::<Vec<_>>()
                    .join(&format!(",\n{padding}"));
                (
                    format!("(insert {typ} {{\n{padding}{values}\n{padding_closing}}})"),
                    args,
                )
            }
        }
    }
}

pub const INITIAL_CONFIG: &str = r###"
## ==== [ Instance configuration ] ====

## This file is applied with `gel init`.
## Generally, it should not be checked into source control.
## It can contain any configuration setting supported by your instance.
## Below is a list of most common and useful settings, commented-out.
## (note: you can embed EdgeQL expressions with {{ … }})

## ---- [ Generic config settings ] ----

[local.config]

## timeouts
# session_idle_transaction_timeout     = "30 seconds"
# query_execution_timeout              = "1 minute"

## which SMTP provider to use by default (see the configuration example below)
# current_email_provider_name       = "mailtrap_sandbox"

## DDL & policy flags
# allow_dml_in_functions             = false
# allow_bare_ddl                     = "NeverAllow"        # "AlwaysAllow" | "NeverAllow"
# allow_user_specified_id            = false
# warn_old_scoping                   = false

## CORS & cache
# cors_allow_origins                  = ["http://localhost:8000", "http://127.0.0.1:8000"]
# auto_rebuild_query_cache            = false
# auto_rebuild_query_cache_timeout    = "30 seconds"
# http_max_connections                = 100

## Email providers (SMTP)
# [[local.config."cfg::SMTPProviderConfig"]]
# name                  = "mailtrap_sandbox"
# sender                = "hello@example.com"
# host                  = "sandbox.smtp.mailtrap.io"
# port                  = 2525
# username              = "YOUR_USERNAME"
# password              = "YOUR_PASSWORD"
# timeout_per_email     = "5 minutes"
# timeout_per_attempt   = "1 minute"
# validate_certs        = false


## ---- [ Auth ] ----

## To use these options, you must first enable `auth` extension in your schema.
## 1. Add `using extension auth;` to default.gel,
## 2. Run `gel migration create`
## 3. Run `gel migration apply`

## general auth settings
# [local.config."ext::auth::AuthConfig"]
# app_name                           = "My Project"
# logo_url                           = "https://localhost:8000/static/logo.png"
# dark_logo_url                      = "https://localhost:8000/static/darklogo.png"
# brand_color                        = "#0000FF"
# auth_signing_key                   = "__GENERATED_UUID__"
# token_time_to_live                 = "1 hour"
# allowed_redirect_urls              = ["http://localhost:8000", "http://testserver"]

## Email & Password Auth Provider
# [[local.config."ext::auth::EmailPasswordProviderConfig"]]
# require_verification              = false

## Apple OAuth Provider
# [[local.config."ext::auth::AppleOAuthProvider"]]
# client_id                         = "YOUR_APPLE_CLIENT_ID"
# secret                            = "YOUR_APPLE_SECRET"
# additional_scope                  = "email name"

## Azure OAuth Provider
# [[local.config."ext::auth::AzureOAuthProvider"]]
# client_id                         = "YOUR_AZURE_CLIENT_ID"
# secret                            = "YOUR_AZURE_SECRET"
# additional_scope                  = "openid profile email"

## Discord OAuth Provider
# [[local.config."ext::auth::DiscordOAuthProvider"]]
# client_id                         = "YOUR_DISCORD_CLIENT_ID"
# secret                            = "YOUR_DISCORD_SECRET"
# additional_scope                  = "identify email"

## Slack OAuth Provider
# [[local.config."ext::auth::SlackOAuthProvider"]]
# client_id                         = "YOUR_SLACK_CLIENT_ID"
# secret                            = "YOUR_SLACK_SECRET"
# additional_scope                  = "identity.basic identity.email"

## GitHub OAuth Provider
# [[local.config."ext::auth::GitHubOAuthProvider"]]
# client_id                         = "YOUR_GITHUB_CLIENT_ID"
# secret                            = "YOUR_GITHUB_SECRET"
# additional_scope                  = "read:user user:email"

## Google OAuth Provider
# [[local.config."ext::auth::GoogleOAuthProvider"]]
# client_id                         = "YOUR_GOOGLE_CLIENT_ID"
# secret                            = "YOUR_GOOGLE_SECRET"
# additional_scope                  = "openid email profile"

## WebAuthn Provider
# [[local.config."ext::auth::WebAuthnProviderConfig"]]
# relying_party_origin              = "https://example.com"
# require_verification              = true

## Magic Link Provider
# [[local.config."ext::auth::MagicLinkProviderConfig"]]
# token_time_to_live                = "15 minutes"

## UI customization
# [local.config."ext::auth::UIConfig"]
# redirect_to                        = "http://localhost:8000/auth/callback"
# redirect_to_on_signup              = "http://localhost:8000/auth/callback?isSignUp=true"

## Webhooks (ext::auth::WebhookConfig)
# [[local.config."ext::auth::WebhookConfig"]]
# url                              = "https://example.com/webhook"
# events                           = ["IdentityCreated", "EmailVerified"]
# signing_secret_key               = "YOUR_WEBHOOK_SECRET"


## ---- [ AI ] ----

## To use these options, you must first enable `ai` extension in your schema.
## 1. Add `using extension ai;` to default.gel,
## 2. Run `gel migration create`
## 3. Run `gel migration apply`

# [local.config."ext::ai::Config"]
# indexer_naptime                    = "5 minutes"

## OpenAI Provider
# [[local.config."ext::ai::OpenAIProviderConfig"]]
# api_url                          = "https://api.openai.com/v1"
# secret                           = "YOUR_API_KEY"
# client_id                        = "optional_client_id"

## Anthropic Provider
# [[local.config."ext::ai::AnthropicProviderConfig"]]
# api_url                          = "https://api.anthropic.com/v1"
# secret                           = "YOUR_API_KEY"
# client_id                        = "optional_client_id"

## Mistral Provider
# [[local.config."ext::ai::MistralProviderConfig"]]
# api_url                          = "https://api.mistral.ai/v1"
# secret                           = "YOUR_API_KEY"
# client_id                        = "optional_client_id"

## Ollama Provider
# [[local.config."ext::ai::OllamaProviderConfig"]]
# api_url                          = "http://localhost:11434/api"
# client_id                        = "optional_client_id"

## Example custom provider: Google Gemini via OpenAI-compatible API
# [[local.config."ext::ai::CustomProviderConfig"]]
# api_url     = "https://generativelanguage.googleapis.com/v1beta/openai"
# secret      = "YOUR_GEMINI_API_KEY"
# client_id   = "YOUR_GEMINI_CLIENT_ID"
# api_style   = "OpenAI"            # "OpenAI" | "Anthropic" | "Ollama"
# name        = "google_gemini"
# display_name = "Google Gemini"
"###;
