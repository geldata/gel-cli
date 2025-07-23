use indexmap::IndexMap;

pub struct Schema(Vec<ModuleSchema>);

pub struct ModuleSchema {
    pub extension_name: Option<String>,
    pub object_types: IndexMap<String, ObjectType>,
}

#[derive(Clone)]
pub struct ObjectType {
    /// Pointer of the object (properties and links)
    pub pointers: IndexMap<String, Pointer>,

    /// When a type is top-level, it's properties are configured with `configure set {obj}::{prop} := ...`
    /// Otherwise, it is configured with `configure insert {obj} := { {prop} := ... };`
    pub is_top_level: bool,

    /// Indicates that this object cannot be identified just by its name. This happens, for example,
    /// when it is used as a link on a multi object. `configure insert {obj}` does not indicate which parent
    /// object this object belongs to. Instead these objects are inserted in a nested insert stmt.
    pub is_non_locatable: bool,

    /// Indicates that the object is used as multi property, so we should expect arrays instead of tables.
    pub is_multi: bool,
}

#[derive(Debug, Clone)]
pub struct Pointer {
    pub target: Typ,

    pub is_required: bool,
    pub is_multi: bool,

    pub description: Option<String>,
    pub deprecated: Option<String>,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Typ {
    Primitive(String),
    Enum { name: String, choices: Vec<String> },
    ObjectRef(String),

    // TODO: this is not really a union, it should be an abstract type and sub types
    Union(Vec<String>),
}

impl Schema {
    pub fn find_object(&self, key: &str) -> Option<(Option<&str>, &ObjectType)> {
        for s in &self.0 {
            if let Some(obj) = s.find_object(key) {
                return Some((s.extension_name.as_deref(), obj));
            }
        }
        None
    }

    fn std(&mut self) -> &mut ModuleSchema {
        self.push(None)
    }

    fn ext(&mut self, extension_name: impl ToString) -> &mut ModuleSchema {
        self.push(Some(extension_name.to_string()))
    }

    fn push(&mut self, extension_name: Option<String>) -> &mut ModuleSchema {
        let schema = ModuleSchema {
            extension_name,
            object_types: Default::default(),
        };
        self.0.push(schema);
        self.0.last_mut().unwrap()
    }
}

impl ModuleSchema {
    pub fn find_object<'s>(&'s self, key: &str) -> Option<&'s ObjectType> {
        self.object_types.get(key)
    }

    fn register(&mut self, name: impl ToString, obj: ObjectType) -> Typ {
        for (_, ptr) in &obj.pointers {
            let mut child_links = Vec::new();

            for target_ref in ptr.target.get_object_refs() {
                let target = self.object_types.get_mut(target_ref).unwrap();
                target.is_top_level = false;
                target.is_multi = ptr.is_multi;

                for (_, ptr) in &target.pointers {
                    child_links.extend(ptr.target.get_object_refs().into_iter().cloned());
                }
            }

            if ptr.is_multi {
                // set is_non_locatable
                let mut descendant_links = child_links;
                while let Some(obj_ref) = descendant_links.pop() {
                    let obj = self.object_types.get_mut(&obj_ref).unwrap();
                    obj.is_non_locatable = true;

                    for (_, ptr) in &obj.pointers {
                        descendant_links.extend(ptr.target.get_object_refs().into_iter().cloned());
                    }
                }
            }
        }

        self.object_types.insert(name.to_string(), obj);
        Typ::ObjectRef(name.to_string())
    }
}

impl ObjectType {
    fn new<I, S>(pointers: I) -> Self
    where
        S: ToString,
        I: IntoIterator<Item = (S, Pointer)>,
    {
        Self {
            pointers: pointers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            is_top_level: true,
            is_multi: false,
            is_non_locatable: false,
        }
    }
}

impl Typ {
    pub fn is_scalar(&self) -> bool {
        match self {
            Typ::Primitive(_) => true,
            Typ::Enum { .. } => true,
            Typ::ObjectRef(_) => false,
            Typ::Union(_) => false,
        }
    }

    pub fn get_object_refs(&self) -> Vec<&String> {
        match self {
            Typ::Primitive(_) => Vec::new(),
            Typ::Enum { .. } => Vec::new(),
            Typ::ObjectRef(r) => vec![r],
            Typ::Union(components) => components.iter().collect(),
        }
    }

    pub fn new_union(objects: impl IntoIterator<Item = Typ>) -> Self {
        Typ::Union(
            objects
                .into_iter()
                .map(|t| match t {
                    Typ::ObjectRef(r) => r,
                    _ => panic!(),
                })
                .collect(),
        )
    }
}

impl std::fmt::Display for Typ {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self, f)
    }
}

impl Pointer {
    fn new(target: Typ) -> Pointer {
        Pointer {
            target,
            is_multi: false,
            is_required: false,
            deprecated: None,
            description: None,
            examples: Vec::new(),
        }
    }

    fn multi(mut self) -> Pointer {
        self.is_multi = true;
        self
    }

    fn required(mut self) -> Pointer {
        self.is_required = true;
        self
    }
}

#[allow(dead_code)]
impl Pointer {
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

trait Optional {
    fn optional(self, key: &'static str) -> impl Iterator<Item = (String, Pointer)> + Clone;
}

impl<I> Optional for I
where
    I: Iterator<Item = (String, Pointer)> + Clone,
{
    fn optional(self, key: &'static str) -> impl Iterator<Item = (String, Pointer)> + Clone {
        self.map(move |(k, mut p)| {
            if k.as_str() == key {
                p.is_required = false;
            }
            (k, p)
        })
    }
}

/// Constructs the schema of most used config options
pub fn default_schema() -> Schema {
    fn primitive(typ: impl ToString) -> Typ {
        Typ::Primitive(typ.to_string())
    }

    fn enumeration<I, S>(name: impl ToString, choices: I) -> Typ
    where
        S: ToString,
        I: IntoIterator<Item = S>,
    {
        Typ::Enum {
            name: name.to_string(),
            choices: choices.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    let mut rv = Schema(Vec::new());
    let schema = rv.ext("auth");

    // auth
    let provider_config = vec![(
        "name".to_string(),
        Pointer::new(primitive("str")).required(),
    )];

    let oauth_provider_config = ObjectType::new(provider_config.clone().into_iter().chain([
        ("secret".into(), Pointer::new(primitive("str")).required()),
        (
            "client_id".into(),
            Pointer::new(primitive("str")).required(),
        ),
        (
            "display_name".into(),
            Pointer::new(primitive("str")).required(),
        ),
        ("additional_scope".into(), Pointer::new(primitive("str"))),
    ]));

    let openid_connect_provider_config = ObjectType::new(
        provider_config
            .clone()
            .into_iter()
            .chain(oauth_provider_config.pointers.clone())
            .chain([
                (
                    "issuer_url".into(),
                    Pointer::new(primitive("str")).required(),
                ),
                ("logo_url".into(), Pointer::new(primitive("str"))),
            ]),
    );
    let vendor_oauth_provider_config = ObjectType::new(
        oauth_provider_config
            .pointers
            .clone()
            .into_iter()
            .optional("name")
            .optional("display_name"),
    );

    let email_password_provider_config = ObjectType::new(vec![
        ("name".to_string(), Pointer::new(primitive("str"))),
        (
            "require_verification".into(),
            Pointer::new(primitive("bool")),
        ),
    ]);
    let web_authn_provider_config = ObjectType::new(vec![
        ("name".to_string(), Pointer::new(primitive("str"))),
        (
            "relying_party_origin".into(),
            Pointer::new(primitive("str")).required(),
        ),
        (
            "require_verification".into(),
            Pointer::new(primitive("bool")),
        ),
    ]);
    let magic_link_provider_config = ObjectType::new(vec![
        ("name".to_string(), Pointer::new(primitive("str"))),
        (
            "token_time_to_live".into(),
            Pointer::new(primitive("duration")),
        ),
    ]);

    let auth_providers = Typ::new_union(vec![
        schema.register("ext::auth::OAuthProviderConfig", oauth_provider_config),
        schema.register(
            "ext::auth::OpenIDConnectProvider",
            openid_connect_provider_config,
        ),
        schema.register(
            "ext::auth::AppleOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        schema.register(
            "ext::auth::AzureOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        schema.register(
            "ext::auth::DiscordOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        schema.register(
            "ext::auth::SlackOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        schema.register(
            "ext::auth::GitHubOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        schema.register(
            "ext::auth::GoogleOAuthProvider",
            vendor_oauth_provider_config.clone(),
        ),
        schema.register(
            "ext::auth::EmailPasswordProviderConfig",
            email_password_provider_config,
        ),
        schema.register(
            "ext::auth::WebAuthnProviderConfig",
            web_authn_provider_config,
        ),
        schema.register(
            "ext::auth::MagicLinkProviderConfig",
            magic_link_provider_config,
        ),
    ]);
    let ui_config = schema.register(
        "ext::auth::UIConfig",
        ObjectType::new([
            ("redirect_to", Pointer::new(primitive("str")).required()),
            ("redirect_to_on_signup", Pointer::new(primitive("str"))),
            (
                "flow_type",
                Pointer::new(enumeration("ext::auth::FlowType", ["PKCE", "implicit"])),
            ),
            ("app_name", Pointer::new(primitive("str"))),
            ("logo_url", Pointer::new(primitive("str"))),
            ("dark_logo_url", Pointer::new(primitive("str"))),
            ("brand_color", Pointer::new(primitive("str"))),
        ]),
    );
    let webhooks_config = schema.register(
        "ext::auth::WebhookConfig",
        ObjectType::new([
            ("url", Pointer::new(primitive("str")).required()),
            (
                "events",
                Pointer::new(enumeration(
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
                ))
                .multi()
                .required(),
            ),
            ("signing_secret_key", Pointer::new(primitive("str"))),
        ]),
    );
    schema.register(
        "ext::auth::AuthConfig",
        ObjectType::new([
            ("providers", Pointer::new(auth_providers).multi()),
            ("ui", Pointer::new(ui_config)),
            ("webhooks", Pointer::new(webhooks_config).multi()),
            ("app_name", Pointer::new(primitive("str"))),
            ("logo_url", Pointer::new(primitive("str"))),
            ("dark_logo_url", Pointer::new(primitive("str"))),
            ("brand_color", Pointer::new(primitive("str"))),
            ("auth_signing_key", Pointer::new(primitive("str"))),
            ("token_time_to_live", Pointer::new(primitive("duration"))),
            (
                "allowed_redirect_urls",
                Pointer::new(primitive("str")).multi(),
            ),
        ]),
    );

    // AI
    let schema = rv.ext("ai");
    let provider_config = vec![
        (
            "name".to_string(),
            Pointer::new(primitive("str")).required(),
        ),
        (
            "display_name".to_string(),
            Pointer::new(primitive("str")).required(),
        ),
        (
            "api_url".to_string(),
            Pointer::new(primitive("str")).required(),
        ),
        ("client_id".to_string(), Pointer::new(primitive("str"))),
        (
            "secret".to_string(),
            Pointer::new(primitive("str")).required(),
        ),
        (
            "api_style".to_string(),
            Pointer::new(enumeration(
                "ext::ai::ProviderAPIStyle",
                ["OpenAI", "Anthropic", "Ollama"],
            ))
            .required(),
        ),
    ];
    let vendor_provider_config = provider_config
        .clone()
        .into_iter()
        .optional("name")
        .optional("display_name")
        .optional("api_url")
        .optional("api_style");

    let ai_providers = Typ::new_union(vec![
        schema.register(
            "ext::ai::CustomProviderConfig",
            ObjectType::new(
                provider_config
                    .clone()
                    .into_iter()
                    .optional("display_name")
                    .optional("api_style"),
            ),
        ),
        schema.register(
            "ext::ai::OpenAIProviderConfig",
            ObjectType::new(vendor_provider_config.clone()),
        ),
        schema.register(
            "ext::ai::MistralProviderConfig",
            ObjectType::new(vendor_provider_config.clone()),
        ),
        schema.register(
            "ext::ai::AnthropicProviderConfig",
            ObjectType::new(vendor_provider_config.clone()),
        ),
        schema.register(
            "ext::ai::OllamaProviderConfig",
            ObjectType::new(vendor_provider_config.optional("secret")),
        ),
    ]);

    schema.register(
        "ext::ai::Config",
        ObjectType::new([
            ("indexer_naptime", Pointer::new(primitive("duration"))),
            ("providers", Pointer::new(ai_providers).multi()),
        ]),
    );

    let schema = rv.std();

    // email provider
    let email_provider_config = vec![("name", Pointer::new(primitive("str")).required())];

    let smtp_provider_config = schema.register(
        "cfg::SMTPProviderConfig",
        ObjectType::new(email_provider_config.clone().into_iter().chain([
            ("sender", Pointer::new(primitive("str"))),
            ("host", Pointer::new(primitive("str"))),
            ("port", Pointer::new(primitive("int32"))),
            ("username", Pointer::new(primitive("str"))),
            ("password", Pointer::new(primitive("str"))),
            (
                "security",
                Pointer::new(enumeration(
                    "cfg::SMTPSecurity",
                    ["PlainText", "TLS", "STARTTLS", "STARTTLSOrPlainText"],
                )),
            ),
            ("validate_certs", Pointer::new(primitive("bool"))),
            ("timeout_per_email", Pointer::new(primitive("duration"))),
            ("timeout_per_attempt", Pointer::new(primitive("duration"))),
        ])),
    );

    // cfg::Auth
    let transport = enumeration("cfg::ConnectionTransport", ["TCP", "TCP_PG", "HTTP"]);
    let cfg_auth_method = Typ::new_union(vec![
        schema.register(
            "cfg::Trust",
            ObjectType::new([("transports", Pointer::new(transport.clone()).multi())]),
        ),
        schema.register(
            "cfg::SCRAM",
            ObjectType::new([("transports", Pointer::new(transport.clone()).multi())]),
        ),
        schema.register(
            "cfg::JWT",
            ObjectType::new([("transports", Pointer::new(transport).multi())]),
        ),
    ]);

    let cfg_auth = schema.register(
        "cfg::Auth",
        ObjectType::new([
            ("priority", Pointer::new(primitive("int64")).required()),
            ("user", Pointer::new(primitive("str")).multi()),
            ("method", Pointer::new(cfg_auth_method).required()),
            ("comment", Pointer::new(primitive("str"))),
        ]),
    );

    schema.register(
        "cfg::Config",
        ObjectType::new([
            (
                "default_transaction_isolation",
                Pointer::new(enumeration(
                    "sys::TransactionIsolation",
                    ["Serializable", "RepeatableRead"],
                )),
            ),
            (
                "default_transaction_access_mode",
                Pointer::new(enumeration(
                    "sys::TransactionAccessMode",
                    ["ReadOnly", "ReadWrite"],
                )),
            ),
            (
                "default_transaction_deferrable",
                Pointer::new(enumeration(
                    "sys::TransactionDeferrability",
                    ["Deferrable", "NotDeferrable"],
                )),
            ),
            (
                "session_idle_transaction_timeout",
                Pointer::new(primitive("duration")),
            ),
            (
                "query_execution_timeout",
                Pointer::new(primitive("duration")),
            ),
            (
                "email_providers",
                Pointer::new(Typ::new_union([smtp_provider_config])).multi(),
            ),
            (
                "current_email_provider_name",
                Pointer::new(primitive("str")),
            ),
            ("allow_dml_in_functions", Pointer::new(primitive("bool"))),
            (
                "allow_bare_ddl",
                Pointer::new(enumeration(
                    "cfg::AllowBareDDL",
                    ["AlwaysAllow", "NeverAllow"],
                )),
            ),
            (
                "store_migration_sdl",
                Pointer::new(enumeration(
                    "cfg::StoreMigrationSDL",
                    ["AlwaysStore", "NeverStore"],
                )),
            ),
            ("apply_access_policies", Pointer::new(primitive("bool"))),
            ("apply_access_policies_pg", Pointer::new(primitive("bool"))),
            ("allow_user_specified_id", Pointer::new(primitive("bool"))),
            ("simple_scoping", Pointer::new(primitive("bool"))),
            ("warn_old_scoping", Pointer::new(primitive("bool"))),
            ("cors_allow_origins", Pointer::new(primitive("str")).multi()),
            ("auto_rebuild_query_cache", Pointer::new(primitive("bool"))),
            (
                "auto_rebuild_query_cache_timeout",
                Pointer::new(primitive("duration")),
            ),
            (
                "query_cache_mode",
                Pointer::new(enumeration(
                    "cfg::QueryCacheMode",
                    ["InMemory", "RegInline", "PgFunc", "Default"],
                )),
            ),
            ("http_max_connections", Pointer::new(primitive("int64"))),
            (
                "track_query_stats",
                Pointer::new(enumeration("cfg::QueryStatsOption", ["None", "All"])),
            ),
            ("auth", Pointer::new(cfg_auth).multi()),
        ]),
    );

    rv
}
