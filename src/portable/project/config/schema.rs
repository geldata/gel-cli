use std::collections::HashMap;

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

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Property {
    pub kind: PropertyKind,
    pub required: bool,
    pub description: Option<String>,
    pub deprecated: Option<String>,
    pub examples: Vec<String>,
}

#[allow(dead_code)]
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

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum PropertyKind {
    Singleton(Schema),
    Array(Schema),
    Multiset(Schema),
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

    pub fn find_object_schema(&self, name: &str) -> Option<(&Self, bool)> {
        match self {
            Schema::Object { typ, .. } if typ == name => Some((self, false)),
            Schema::Object { members, .. } => members.values().find_map(|prop| match &prop.kind {
                PropertyKind::Singleton(schema) => schema.find_object_schema(name),
                PropertyKind::Multiset(schema) => {
                    schema.find_object_schema(name).map(|(s, _)| (s, true))
                }
                _ => None,
            }),
            Schema::Union(schemas) => schemas.iter().find_map(|s| s.find_object_schema(name)),
            _ => None,
        }
    }
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
    use PropertyKind::*;
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
        "ext::auth::WebhookConfig",
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
