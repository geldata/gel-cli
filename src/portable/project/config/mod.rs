mod schema;
mod validation;

use clap::ValueHint;
use gel_protocol::value::Value as GelValue;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::path;
use toml::Value as TomlValue;

use crate::branding::{BRANDING_CLI_CMD, QUERY_TAG};
use crate::commands::ExitCode;
use crate::connect::Connection;
use crate::options::Options;
use crate::print::{self, Highlight};
use validation::Commands;

#[derive(clap::Args, Clone, Debug)]
pub struct Command {
    #[arg(long, value_hint=ValueHint::DirPath)]
    pub project_dir: Option<path::PathBuf>,
}

pub fn run(c: &Command, _options: &Options) -> anyhow::Result<()> {
    let project_loc = super::find_project(c.project_dir.as_ref().map(|p| p.as_ref()))?;

    let Some(project_loc) = project_loc else {
        print::msg!(
            "{} {} Run `{BRANDING_CLI_CMD} project init`.",
            print::err_marker(),
            "Project is not initialized.".emphasized()
        );
        return Err(ExitCode::new(1).into());
    };

    apply_local(&project_loc.root)
}

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
    let commands = validation::validate(config, &schema)?;
    if commands.is_empty() {
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
    configure(&mut conn, &commands).await?;
    conn.execute("COMMIT;", &()).await?;

    print::msg!("Done.");

    Ok(())
}

async fn configure(conn: &mut Connection, commands: &Commands) -> anyhow::Result<()> {
    for (cfg_obj, prop, value) in &commands.set {
        set_value(conn, cfg_obj, prop, value).await?;
    }
    for (cfg_object, inserts) in &commands.insert {
        execute_configure(conn, &format!("reset {cfg_object}")).await?;
        for values in inserts {
            let (query, args) = compile_insert(&cfg_object, values, 1);
            execute_configure_args(conn, &query, args).await?;
        }
    }
    Ok(())
}

async fn set_value(
    conn: &mut Connection,
    config_object: &str,
    property: &str,
    value: &Value,
) -> anyhow::Result<()> {
    let config_object = match config_object {
        "cfg::Config" => "cfg",
        config => config,
    };

    let (value, args) = compile_value(value, 1);

    let query = format!("set {config_object}::{property} := {value}");
    execute_configure_args(conn, &query, args).await?;
    Ok(())
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

fn compile_value(value: &Value, indent: usize) -> (String, HashMap<String, GelValue>) {
    let padding = " ".repeat((indent + 1) * 2);
    let padding_closing = " ".repeat(indent * 2);

    match value {
        Value::Injected(value) => (value.clone(), HashMap::new()),
        Value::Set(values) => {
            let mut params = Vec::new();
            let mut args = HashMap::new();

            for value in values {
                let (p, a) = compile_value(value, indent + 1);
                params.push(p);
                args.extend(a);
            }
            let params = params.join(&format!(",\n{padding}"));

            (format!("{{\n{padding}{params}\n{padding_closing}}}"), args)
        }
        Value::Array(values) => {
            let mut params = Vec::new();
            let mut args = HashMap::new();

            for value in values {
                let (p, a) = compile_value(value, indent + 1);
                params.push(p);
                args.extend(a);
            }
            let params = params.join(&format!(",\n{padding}"));

            (format!("[\n{padding}{params}\n{padding_closing}]"), args)
        }
        Value::Insert { typ, values } => {
            let (query, args) = compile_insert(typ, values, indent + 1);
            (format!("({query})"), args)
        }
    }
}

fn compile_insert(
    cfg_obj: &str,
    values: &IndexMap<String, Value>,
    indent: usize,
) -> (String, HashMap<String, GelValue>) {
    let padding = " ".repeat((indent + 1) * 2);
    let padding_closing = " ".repeat(indent * 2);

    let mut params = Vec::new();
    let mut args = HashMap::new();

    for (name, val) in values {
        let (p, a) = compile_value(val, indent + 1);
        params.push(format!("{name} := {p}"));
        args.extend(a);
    }
    let params = params.join(&format!(",\n{padding}"));

    (
        format!("insert {cfg_obj} {{\n{padding}{params}\n{padding_closing}}}"),
        args,
    )
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
#[allow(dead_code)]
pub enum Value {
    Injected(String),
    Array(Vec<Value>), // array is not really used, we don't have such config, right?
    Set(Vec<Value>),
    Insert {
        typ: String,
        values: IndexMap<String, Value>,
    },
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
