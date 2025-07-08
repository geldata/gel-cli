use anyhow::Context;
use edgeql_parser::helpers::quote_string as ql;
use gel_protocol::value::Value as GelValue;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use toml::Value as TomlValue;

use crate::connect::Connection;

pub async fn sync_config(local_toml: &PathBuf, conn: &mut Connection) -> anyhow::Result<()> {
    if local_toml.exists() {
        // read toml
        let local_conf = tokio::fs::read_to_string(local_toml).await?;
        let toml = toml::de::Deserializer::new(&local_conf);
        let local_conf: LocalConfig = serde_path_to_error::deserialize(toml)?;

        // configure
        let config = local_conf.local.and_then(|l| l.config);
        if let Some(config) = config {
            conn.execute("START TRANSACTION;", &()).await?;
            default_schema().configure(conn, config).await?;
            conn.execute("COMMIT;", &()).await?;
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum Value {
    Injected(String),
    Array(Vec<Value>),
    Set(Vec<Value>),
    Nested {
        typ: String,
        values: HashMap<String, Value>,
    },
}

impl TryFrom<TomlValue> for Value {
    type Error = anyhow::Error;

    fn try_from(value: TomlValue) -> anyhow::Result<Self> {
        Ok(match value {
            TomlValue::String(value) => {
                match value.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")) {
                    Some(value) => Value::Injected(value.to_string()),
                    None => Value::Injected(value.into()),
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
                Self::Nested { typ, values }
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

    async fn set(self, conn: &mut Connection, config: &str, name: &str) -> anyhow::Result<()> {
        let config = match config {
            "cfg::Config" => "cfg",
            config => config,
        };
        match self {
            Value::Injected(value) => {
                let query = format!("configure current branch set {config}::{name} := {value};");
                println!("Executing query: {query}");
                conn.execute(&query, &()).await?;
            }
            Value::Set(values) => {
                let mut args = HashMap::new();
                let values = values
                    .into_iter()
                    .map(|value| {
                        let (value, value_args) = value.compile(args.len());
                        args.extend(value_args);
                        value
                    })
                    .collect::<Vec<_>>()
                    .join(",\n\t");
                let query =
                    format!("configure current branch set {config}::{name} := {{\n\t{values}\n}};");
                println!("Executing query: {query}\n\twith args: {args:?}");
                let args = args
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.clone().into()))
                    .collect::<HashMap<_, gel_protocol::value_opt::ValueOpt>>();
                conn.execute(&query, &args).await?;
            }
            Value::Array(values) => {
                let mut args = HashMap::new();
                let values = values
                    .into_iter()
                    .map(|value| {
                        let (value, value_args) = value.compile(args.len());
                        args.extend(value_args);
                        value
                    })
                    .collect::<Vec<_>>()
                    .join(",\n\t");
                let query =
                    format!("configure current branch set {config}::{name} := [\n\t{values}\n];");
                println!("Executing query: {query}\n\twith args: {args:?}");
                let args = args
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.clone().into()))
                    .collect::<HashMap<_, gel_protocol::value_opt::ValueOpt>>();
                conn.execute(&query, &args).await?;
            }
            _ => {
                anyhow::bail!("Unsupported value type for setting: {self:?}");
            }
        }
        Ok(())
    }

    fn compile(self, _arg_index: usize) -> (String, HashMap<String, GelValue>) {
        match self {
            Value::Injected(value) => (value, HashMap::new()),
            _ => {
                panic!("Unsupported value type to compile: {self:?}");
            }
        }
    }

    async fn insert(self, conn: &mut Connection) -> anyhow::Result<()> {
        let Value::Nested { typ, values } = self else {
            anyhow::bail!("Unsupported value type for inserting: {self:?}");
        };
        let mut args = HashMap::new();
        let values = values
            .into_iter()
            .map(|(name, value)| {
                let (value, value_args) = value.compile(args.len());
                args.extend(value_args);
                format!("{name} := {value}")
            })
            .collect::<Vec<_>>()
            .join(",\n\t");
        let query = format!("configure current branch insert {typ} {{\n\t{values}\n}};");
        println!("Executing query: {query}");
        let args = args
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone().into()))
            .collect::<HashMap<_, gel_protocol::value_opt::ValueOpt>>();
        conn.execute(&query, &args).await?;
        Ok(())
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct LocalConfig {
    local: Option<Local>,
}

#[derive(Debug, serde::Deserialize)]
pub struct Local {
    config: Option<TomlValue>,
}

#[derive(Clone, Debug)]
pub enum Kind {
    Singleton(Schema),
    Array(Schema),
    Multiset(Schema),
}

#[derive(Clone, Debug)]
pub struct Property {
    kind: Kind,
    required: bool,
    description: Option<String>,
    deprecated: Option<String>,
    examples: Vec<String>,
}

impl Property {
    fn with_description(mut self, description: impl ToString) -> Self {
        self.description = Some(description.to_string());
        self
    }

    fn with_deprecated(mut self, deprecated: impl ToString) -> Self {
        self.deprecated = Some(deprecated.to_string());
        self
    }

    fn with_examples<I, S>(mut self, examples: I) -> Self
    where
        S: ToString,
        I: IntoIterator<Item = S>,
    {
        self.examples = examples.into_iter().map(|s| s.to_string()).collect();
        self
    }
}

/// The schema of configuration values
#[derive(Clone, Debug)]
pub enum Schema {
    Primitive {
        typ: String,
    },
    Enum {
        typ: String,
        choices: Vec<String>,
    },
    Object {
        typ: String,
        members: HashMap<String, Property>,
    },
    Union(Vec<Schema>),
}

trait Optional {
    fn optional(self, key: &str) -> Vec<(String, Property)>;
}

impl<S> Optional for Vec<(S, Property)>
where
    S: ToString,
{
    fn optional(self, key: &str) -> Vec<(String, Property)> {
        self.into_iter()
            .map(|(k, v)| {
                let k = k.to_string();
                if k != key {
                    return (k, v);
                }
                (
                    k,
                    Property {
                        required: false,
                        ..v
                    },
                )
            })
            .collect()
    }
}

/// Constructs the schema of most used config options
pub fn default_schema() -> Schema {
    use Kind::*;
    use Schema::*;

    fn primitive(typ: impl ToString) -> Schema {
        Primitive {
            typ: typ.to_string(),
        }
    }

    fn enumeration<I, S>(typ: impl ToString, choices: I) -> Schema
    where
        S: ToString,
        I: IntoIterator<Item = S>,
    {
        Enum {
            typ: typ.to_string(),
            choices: choices.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    fn singleton(schema: Schema, required: bool) -> Property {
        Property {
            kind: Singleton(schema),
            required,
            description: None,
            deprecated: None,
            examples: vec![],
        }
    }

    fn object<I, S>(typ: impl ToString, members: I) -> Schema
    where
        S: ToString,
        I: IntoIterator<Item = (S, Property)>,
    {
        Object {
            typ: typ.to_string(),
            members: members
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    fn multiset(schema: Schema, required: bool) -> Property {
        Property {
            kind: Multiset(schema),
            required,
            description: None,
            deprecated: None,
            examples: vec![],
        }
    }

    let provider_config = vec![("name", singleton(primitive("str"), true))];
    let oauth_provider_config = provider_config
        .clone()
        .into_iter()
        .chain([
            ("secret".into(), singleton(primitive("str"), true)),
            ("client_id".into(), singleton(primitive("str"), true)),
            ("display_name".into(), singleton(primitive("str"), true)),
            (
                "additional_scope".into(),
                singleton(primitive("str"), false),
            ),
        ])
        .collect::<Vec<_>>();
    let openid_connect_provider_config = provider_config
        .clone()
        .into_iter()
        .chain(oauth_provider_config.clone())
        .chain([
            ("issuer_url".into(), singleton(primitive("str"), true)),
            ("logo_url".into(), singleton(primitive("str"), false)),
        ]);
    let vendor_oauth_provider_config = oauth_provider_config
        .clone()
        .optional("name")
        .optional("display_name");
    let provider_config_optional_name = provider_config.clone().optional("name");
    let email_password_provider_config =
        provider_config_optional_name.clone().into_iter().chain([(
            "require_verification".into(),
            singleton(primitive("bool"), false),
        )]);
    let web_authn_provider_config = provider_config_optional_name.clone().into_iter().chain([
        (
            "relying_party_origin".into(),
            singleton(primitive("str"), true),
        ),
        (
            "require_verification".into(),
            singleton(primitive("bool"), false),
        ),
    ]);
    let magic_link_provider_config = provider_config_optional_name.clone().into_iter().chain([(
        "token_time_to_live".into(),
        singleton(primitive("duration"), false),
    )]);
    let auth_providers = vec![
        object("ext::auth::OAuthProviderConfig", oauth_provider_config),
        object(
            "ext::auth::OpenIDConnectProvider",
            openid_connect_provider_config,
        ),
        object(
            "ext::auth::AppleOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        object(
            "ext::auth::AzureOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        object(
            "ext::auth::DiscordOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        object(
            "ext::auth::SlackOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        object(
            "ext::auth::GitHubOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        object(
            "ext::auth::GoogleOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        object(
            "ext::auth::EmailPasswordProviderConfig",
            email_password_provider_config,
        ),
        object(
            "ext::auth::WebAuthnProviderConfig",
            web_authn_provider_config,
        ),
        object(
            "ext::auth::MagicLinkProviderConfig",
            magic_link_provider_config,
        ),
    ];
    let ui_config = object(
        "ext::auth::UIConfig",
        [
            ("redirect_to", singleton(primitive("str"), true)),
            ("redirect_to_on_signup", singleton(primitive("str"), false)),
            (
                "flow_type",
                singleton(
                    enumeration("ext::auth::FlowType", ["PKCE", "implicit"]),
                    false,
                ),
            ),
            ("app_name".into(), singleton(primitive("str"), false)),
            ("logo_url".into(), singleton(primitive("str"), false)),
            ("dark_logo_url".into(), singleton(primitive("str"), false)),
            ("brand_color".into(), singleton(primitive("str"), false)),
        ],
    );
    let webhook_event = enumeration(
        "ext::auth::WebhookEvent",
        [
            "IdentityCreated",
            "IdentityAuthenticated",
            "EmailFactorCreated",
            "EmailVerified",
            "EmailVerificationRequested",
            "PasswordResetRequested",
            "MagicLinkRequested",
        ],
    );
    let webhook_config = object(
        "ext::ai::WebhookConfig",
        [
            ("url", singleton(primitive("str"), true)),
            ("events", multiset(webhook_event, true)),
            (
                "signing_secret_key".into(),
                singleton(primitive("str"), false),
            ),
        ],
    );
    let auth = object(
        "ext::auth::AuthConfig",
        [
            ("providers", multiset(Union(auth_providers), false)),
            ("ui", singleton(ui_config, false)),
            ("webhooks", multiset(webhook_config, false)),
            ("app_name", singleton(primitive("str"), false)),
            ("logo_url", singleton(primitive("str"), false)),
            ("dark_logo_url", singleton(primitive("str"), false)),
            ("brand_color", singleton(primitive("str"), false)),
            ("auth_signing_key", singleton(primitive("str"), false)),
            (
                "token_time_to_live",
                singleton(primitive("duration"), false),
            ),
            ("allowed_redirect_urls", multiset(primitive("str"), false)),
        ],
    );
    let provider_api_style = enumeration(
        "ext::ai::ProviderAPIStyle",
        ["OpenAI", "Anthropic", "Ollama"],
    );
    let provider_config = vec![
        ("name", singleton(primitive("str"), true)),
        ("display_name", singleton(primitive("str"), true)),
        ("api_url", singleton(primitive("str"), true)),
        ("client_id", singleton(primitive("str"), false)),
        ("secret", singleton(primitive("str"), true)),
        ("api_style", singleton(provider_api_style, true)),
    ];
    let vendor_provider_config = provider_config
        .clone()
        .optional("name")
        .optional("display_name")
        .optional("api_url")
        .optional("api_style");
    let ai_providers = vec![
        object(
            "ext::ai::CustomProviderConfig",
            provider_config
                .clone()
                .optional("display_name")
                .optional("api_style"),
        ),
        object(
            "ext::ai::OpenAIProviderConfig",
            vendor_provider_config.clone(),
        ),
        object(
            "ext::ai::MistralProviderConfig",
            vendor_provider_config.clone(),
        ),
        object(
            "ext::ai::AnthropicProviderConfig",
            vendor_provider_config.clone(),
        ),
        object(
            "ext::ai::OllamaProviderConfig",
            vendor_provider_config.clone().optional("secret"),
        ),
    ];
    let ai = object(
        "ext::ai::Config",
        [
            ("indexer_naptime", singleton(primitive("duration"), false)),
            ("providers", multiset(Union(ai_providers), false)),
        ],
    );
    let extensions = object(
        "cfg::ExtensionConfig",
        [
            ("auth", singleton(auth, false)),
            ("ai", singleton(ai, false)),
        ],
    );
    let transaction_isolation = enumeration(
        "sys::TransactionIsolation",
        ["Serializable", "RepeatableRead"],
    );
    let transaction_access_mode =
        enumeration("sys::TransactionAccessMode", ["ReadOnly", "ReadWrite"]);
    let transaction_deferrability = enumeration(
        "sys::TransactionDeferrability",
        ["Deferrable", "NotDeferrable"],
    );
    let email_provider_config = vec![("name", singleton(primitive("str"), true))];
    let smtp_security = enumeration(
        "cfg::SMTPSecurity",
        ["PlainText", "TLS", "STARTTLS", "STARTTLSOrPlainText"],
    );
    let smtp_provider_config = email_provider_config.clone().into_iter().chain([
        ("sender".into(), singleton(primitive("str"), false)),
        ("host".into(), singleton(primitive("str"), false)),
        ("port".into(), singleton(primitive("int32"), false)),
        ("username".into(), singleton(primitive("str"), false)),
        ("password".into(), singleton(primitive("str"), false)),
        ("security".into(), singleton(smtp_security, false)),
        ("validate_certs".into(), singleton(primitive("bool"), false)),
        (
            "timeout_per_email".into(),
            singleton(primitive("duration"), false),
        ),
        (
            "timeout_per_attempt".into(),
            singleton(primitive("duration"), false),
        ),
    ]);
    let email_providers = vec![object("cfg::SMTPProviderConfig", smtp_provider_config)];
    let allow_bare_ddl = enumeration("cfg::AllowBareDDL", ["AlwaysAllow", "NeverAllow"]);
    let store_migration_sdl = enumeration("cfg::StoreMigrationSDL", ["AlwaysStore", "NeverStore"]);
    let query_cache_mode = enumeration(
        "cfg::QueryCacheMode",
        ["InMemory", "RegInline", "PgFunc", "Default"],
    );
    let query_stats_option = enumeration("cfg::QueryStatsOption", ["None", "All"]);
    object(
        "cfg::Config",
        [
            ("extensions", singleton(extensions, false)),
            (
                "default_transaction_isolation",
                singleton(transaction_isolation, false),
            ),
            (
                "default_transaction_access_mode",
                singleton(transaction_access_mode, false),
            ),
            (
                "default_transaction_deferrable",
                singleton(transaction_deferrability, false),
            ),
            (
                "session_idle_transaction_timeout",
                singleton(primitive("duration"), false),
            ),
            (
                "query_execution_timeout",
                singleton(primitive("duration"), false),
            ),
            ("email_providers", multiset(Union(email_providers), false)),
            (
                "current_email_provider_name",
                singleton(primitive("str"), false),
            ),
            (
                "allow_dml_in_functions",
                singleton(primitive("bool"), false),
            ),
            ("allow_bare_ddl", singleton(allow_bare_ddl, false)),
            ("store_migration_sdl", singleton(store_migration_sdl, false)),
            ("apply_access_policies", singleton(primitive("bool"), false)),
            (
                "apply_access_policies_pg",
                singleton(primitive("bool"), false),
            ),
            (
                "allow_user_specified_id",
                singleton(primitive("bool"), false),
            ),
            ("simple_scoping", singleton(primitive("bool"), false)),
            ("warn_old_scoping", singleton(primitive("bool"), false)),
            ("cors_allow_origins", multiset(primitive("str"), false)),
            (
                "auto_rebuild_query_cache",
                singleton(primitive("bool"), false),
            ),
            (
                "auto_rebuild_query_cache_timeout",
                singleton(primitive("duration"), false),
            ),
            ("query_cache_mode", singleton(query_cache_mode, false)),
            ("http_max_connections", singleton(primitive("int64"), false)),
            ("track_query_stats", singleton(query_stats_option, false)),
        ],
    )
}

impl Schema {
    pub fn is_scalar(&self) -> bool {
        use Schema::*;
        match self {
            Primitive { .. } | Enum { .. } => true,
            Object { .. } => false,
            Union(schemas) => schemas.iter().all(Schema::is_scalar),
        }
    }

    pub fn get_type(&self) -> Option<&str> {
        use Schema::*;
        match self {
            Primitive { typ } => Some(typ),
            Enum { typ, .. } => Some(typ),
            Object { typ, .. } => Some(typ),
            Union(_) => None,
        }
    }

    fn find_object_schema(&self, name: &str) -> Option<(&Self, bool)> {
        match self {
            Schema::Object { typ, .. } if typ == name => Some((self, false)),
            Schema::Object { members, .. } => members.values().find_map(|prop| match &prop.kind {
                Kind::Singleton(schema) => schema.find_object_schema(name),
                Kind::Multiset(schema) => schema.find_object_schema(name).map(|(s, _)| (s, true)),
                _ => None,
            }),
            Schema::Union(schemas) => schemas.iter().find_map(|s| s.find_object_schema(name)),
            _ => None,
        }
    }

    /// Compares the schema to the toml value and makes sure that the value is of correct type.
    pub fn validate(&self, value: TomlValue, path: &[&str]) -> anyhow::Result<ValidateResult> {
        use Schema::*;
        use TomlValue as Toml;

        match (self, value) {
            (_, Toml::String(value)) if value.starts_with("{{") && value.ends_with("}}") => {
                Ok(Value::Injected(
                    value
                        .strip_prefix("{{")
                        .and_then(|s| s.strip_suffix("}}"))
                        .unwrap()
                        .to_string(),
                )
                .into())
            }
            (Primitive { typ } | Enum { typ, .. }, Toml::Array(_)) => {
                Err(anyhow::anyhow!("expected {typ} but got array"))
            }
            (Primitive { typ } | Enum { typ, .. }, Toml::Table(_)) => {
                Err(anyhow::anyhow!("expected {typ} but got table"))
            }
            (Primitive { typ }, value) => match (typ.as_str(), value) {
                ("str", Toml::String(value)) => Ok(Value::Injected(ql(&value)).into()),
                ("int64", Toml::Integer(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("int32", Toml::Integer(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("int16", Toml::Integer(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("float64", Toml::Float(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("float32", Toml::Float(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("bool", Toml::Boolean(value)) => Ok(Value::Injected(value.to_string()).into()),
                ("duration", Toml::String(value)) => {
                    Ok(Value::Injected(format!("<duration>{}", ql(&value))).into())
                }
                (_, value) => Err(anyhow::anyhow!("expected {typ} but got {value:?}")),
            },
            (Enum { typ, choices }, Toml::String(value)) => {
                if choices.contains(&value) {
                    Ok(Value::Injected(format!("<{}>{}", typ, ql(&value))).into())
                } else {
                    Err(anyhow::anyhow!(
                        "expected one of {choices:?} but got {value}"
                    ))
                }
            }
            (Object { .. }, Toml::Table(value)) => Ok(self.validate_object(value, path)?),
            (Union(schemas), Toml::Table(mut value)) => {
                if let Some(Toml::String(tname)) = value.remove("_tname") {
                    for schema in schemas {
                        if schema.get_type() == Some(&tname) {
                            return schema.validate_object(value, path);
                        }
                    }
                    Err(anyhow::anyhow!("unknown type in union: {tname}"))
                } else {
                    Err(anyhow::anyhow!("expected _tname field in union object"))
                }
            }
            (schema, value) => Err(anyhow::anyhow!("expected {schema:?}, value: {value:?}")),
        }
        .with_context(|| path.join("."))
    }
    fn validate_array(&self, value: TomlValue, path: &[&str]) -> anyhow::Result<Vec<Value>> {
        let ctx = || path.join(".");
        let TomlValue::Array(array) = value else {
            return Err(anyhow::anyhow!("expected array")).with_context(ctx);
        };
        Ok(array
            .into_iter()
            .enumerate()
            .map(|(i, v)| {
                let i = i.to_string();
                let sub_path = &[path, &[&i]].concat();
                self.validate(v, sub_path)
                    .and_then(|r| r.take_result().with_context(|| sub_path.join(".")))
            })
            .collect::<anyhow::Result<_>>()?)
    }
    fn validate_object_array(
        &self,
        array: toml::value::Array,
        path: &[&str],
        flat_config: &mut HashMap<String, Value>,
    ) -> anyhow::Result<Vec<Value>> {
        array
            .into_iter()
            .enumerate()
            .map(|(i, value)| {
                let i = i.to_string();
                let sub_path = &[path, &[&i]].concat();
                self.validate(value, sub_path).and_then(|v| {
                    v.merge_into(flat_config)
                        .with_context(|| sub_path.join("."))
                })
            })
            .collect()
    }

    fn validate_object(
        &self,
        value: toml::value::Table,
        path: &[&str],
    ) -> anyhow::Result<ValidateResult> {
        use Kind::*;

        let Schema::Object { typ, members } = self else {
            panic!("{}: expected object schema", path.join("."));
        };
        let mut flat_config = HashMap::new();
        let mut values = HashMap::new();

        for (key, value) in value {
            let sub_path = &[path, &[&key]].concat();
            let key = key.clone();
            let sub_ctx = || sub_path.join(".");
            match members.get(&key) {
                Some(Property {
                    kind: Singleton(schema),
                    ..
                }) if schema.is_scalar() => {
                    values.insert(
                        key,
                        schema
                            .validate(value, sub_path)?
                            .take_result()
                            .with_context(sub_ctx)?,
                    );
                }
                Some(Property {
                    kind: Singleton(schema),
                    ..
                }) => schema
                    .validate(value, sub_path)?
                    .merge_object(&mut flat_config)
                    .with_context(sub_ctx)?,
                Some(Property {
                    kind: Array(schema),
                    ..
                }) => {
                    values.insert(key, Value::Array(schema.validate_array(value, sub_path)?));
                }
                Some(Property {
                    kind: Multiset(schema),
                    ..
                }) if schema.is_scalar() => {
                    values.insert(key, Value::Set(schema.validate_array(value, sub_path)?));
                }
                Some(Property {
                    kind: Multiset(schema),
                    ..
                }) => {
                    let TomlValue::Array(array) = value else {
                        return Err(anyhow::anyhow!("expected array for multiset"))
                            .with_context(sub_ctx);
                    };
                    let objs = schema.validate_object_array(array, sub_path, &mut flat_config)?;
                    merge_flat_objects(&mut flat_config, sub_path, objs)?;
                }
                None if path.is_empty() => match (self.find_object_schema(&key), value) {
                    (Some((schema, true)), TomlValue::Array(array)) => {
                        let objs =
                            schema.validate_object_array(array, sub_path, &mut flat_config)?;
                        merge_flat_objects(&mut flat_config, sub_path, objs)?;
                    }
                    (Some((schema, false)), TomlValue::Table(table)) => schema
                        .validate_object(table, sub_path)?
                        .merge_object(&mut flat_config)
                        .with_context(sub_ctx)?,
                    (Some((_, multi)), _) => {
                        let expect = if multi { "array" } else { "object" };
                        return Err(anyhow::anyhow!("expected {expect}")).with_context(sub_ctx);
                    }
                    (None, value) => {
                        if let Some(value) =
                            merge_schemaless_value(value, &mut flat_config, sub_path)
                                .with_context(sub_ctx)?
                        {
                            values.insert(key, value);
                        }
                    }
                },
                None => {
                    if let Some(value) = merge_schemaless_value(value, &mut flat_config, sub_path)
                        .with_context(sub_ctx)?
                    {
                        values.insert(key, value);
                    }
                }
            }
        }
        let typ = typ.into();
        Ok(ValidateResult::new(
            Value::Nested { typ, values },
            flat_config,
        ))
    }

    async fn configure(&self, conn: &mut Connection, value: TomlValue) -> anyhow::Result<()> {
        // validate
        let ValidateResult {
            result,
            flat_config: Some(mut flat_config),
        } = self.validate(value, &[])?
        else {
            return Ok(());
        };

        merge_flat_config(
            &mut flat_config,
            [(
                self.get_type()
                    .expect("root schema must have type")
                    .to_string(),
                result,
            )],
        )?;
        for (name, value) in flat_config {
            match value {
                Value::Nested { values, .. } => {
                    for (key, value) in values {
                        value.set(conn, &name, &key).await?;
                    }
                }
                Value::Set(values) => {
                    let query = format!("configure current branch reset {name};");
                    println!("Executing query: {query}");
                    for value in values {
                        value.insert(conn).await?;
                    }
                }
                _ => {
                    panic!("expected object or set, got {value:?}");
                }
            }
        }
        Ok(())
    }
}

pub struct ValidateResult {
    result: Value,
    flat_config: Option<HashMap<String, Value>>,
}

impl From<Value> for ValidateResult {
    fn from(value: Value) -> Self {
        ValidateResult {
            result: value,
            flat_config: None,
        }
    }
}

impl ValidateResult {
    fn new(result: Value, flat_config: HashMap<String, Value>) -> Self {
        ValidateResult {
            result,
            flat_config: Some(flat_config),
        }
    }

    pub fn take_result(self) -> anyhow::Result<Value> {
        if self.flat_config.is_some() {
            anyhow::bail!("not a value-only result");
        }
        Ok(self.result)
    }

    pub fn merge_into(self, flat_config: &mut HashMap<String, Value>) -> anyhow::Result<Value> {
        if let Some(new_flat_config) = self.flat_config {
            merge_flat_config(flat_config, new_flat_config)?;
        }
        Ok(self.result)
    }

    pub fn merge_object(self, flat_config: &mut HashMap<String, Value>) -> anyhow::Result<()> {
        let value = self.merge_into(flat_config)?;
        if let Value::Nested { typ, .. } = &value {
            merge_flat_config(flat_config, [(typ.clone(), value)])
        } else {
            anyhow::bail!("expected object");
        }
    }
}

fn merge_flat_config(
    flat_config: &mut HashMap<String, Value>,
    new_flat_config: impl IntoIterator<Item = (String, Value)>,
) -> anyhow::Result<()> {
    for (key, value) in new_flat_config {
        match flat_config.entry(key) {
            Entry::Occupied(mut entry) => match (entry.get_mut(), value) {
                (
                    Value::Nested {
                        typ: existing_typ,
                        values: existing_map,
                    },
                    Value::Nested {
                        typ: new_typ,
                        values: new_map,
                    },
                ) => {
                    if new_typ.ne(existing_typ) {
                        anyhow::bail!(
                            "cannot merge nested values of different types: {} and {}",
                            existing_typ,
                            new_typ
                        );
                    }
                    for (new_key, new_value) in new_map {
                        match existing_map.entry(new_key) {
                            Entry::Occupied(entry) => {
                                anyhow::bail!("duplicate key: {}", entry.key());
                            }
                            Entry::Vacant(entry) => {
                                entry.insert(new_value);
                            }
                        }
                    }
                }
                (Value::Set(existing_set), Value::Set(new_set)) => {
                    existing_set.extend(new_set);
                }
                (existing, new) => {
                    panic!("expected {existing:?} but got {new:?}");
                }
            },
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
        }
    }
    Ok(())
}

fn merge_flat_objects<I>(
    flat_config: &mut HashMap<String, Value>,
    path: &[&str],
    objects: I,
) -> anyhow::Result<()>
where
    I: IntoIterator<Item = Value>,
{
    let mut values = HashMap::new();
    for (i, value) in objects.into_iter().enumerate() {
        let i = i.to_string();
        let sub_path = &[path, &[&i]].concat();
        if let Value::Nested { typ, .. } = &value {
            values
                .entry(typ.to_string())
                .or_insert_with(|| Vec::new())
                .push(value);
        } else {
            panic!("{}: expected object", sub_path.join("."));
        };
    }
    merge_flat_config(
        flat_config,
        values.into_iter().map(|(k, v)| (k, Value::Set(v))),
    )
    .with_context(|| path.join("."))
}

fn merge_schemaless_value(
    value: TomlValue,
    flat_config: &mut HashMap<String, Value>,
    path: &[&str],
) -> anyhow::Result<Option<Value>> {
    let value = Value::try_from(value)?;
    match value {
        Value::Nested { ref typ, .. } => {
            merge_flat_config(flat_config, [(typ.clone(), Value::try_from(value)?)])
                .with_context(|| path.join("."))?;
            Ok(None)
        }
        Value::Set(items) if items.iter().any(Value::is_object) => {
            merge_flat_objects(flat_config, path, items)?;
            Ok(None)
        }
        _ => Ok(Some(value)),
    }
}
