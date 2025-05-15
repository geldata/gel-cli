use std::path::PathBuf;

use gel_tokio::dsn::CloudCerts;

macro_rules! define_env {
    (
        $(
            #[doc=$doc:expr]
            #[env($($env_name:expr),+)]
            $(#[preprocess=$preprocess:expr])?
            $(#[parse=$parse:expr])?
            $(#[validate=$validate:expr])?
            $name:ident: $type:ty
        ),* $(,)?
    ) => {
        #[derive(Debug, Clone)]
        pub struct Env {
        }

        #[allow(clippy::diverging_sub_expression)]
        impl Env {
            $(
                #[doc = $doc]
                pub fn $name() -> ::std::result::Result<::std::option::Option<$type>, anyhow::Error> {
                    const ENV_NAMES: &[&str] = &[$(stringify!($env_name)),+];
                    let Some((_name, s)) = $crate::cli::env::get_envs(ENV_NAMES)? else {
                        return Ok(None);
                    };
                    $(let Some(s) = $preprocess(&s, context)? else {
                        return Ok(None);
                    };)?

                    // This construct lets us choose between $parse and std::str::FromStr
                    // without requiring all types to implement FromStr.
                    #[allow(unused_labels)]
                    let value: $type = 'block: {
                        $(
                            break 'block $parse(&name, &s)?;

                            // Disable the fallback parser
                            #[cfg(all(debug_assertions, not(debug_assertions)))]
                        )?
                        $crate::cli::env::parse::<_>(&s)?
                    };

                    $($validate(name, &value)?;)?
                    Ok(Some(value))
                }
            )*
        }
    };
}

define_env! {
    /// Path to the editor executable
    #[env(GEL_EDITOR, EDGEDB_EDITOR)]
    editor: String,

    /// Whether to install in Docker
    #[env(GEL_INSTALL_IN_DOCKER, EDGEDB_INSTALL_IN_DOCKER)]
    install_in_docker: InstallInDocker,

    /// Development server directory path
    #[env(GEL_SERVER_DEV_DIR, EDGEDB_SERVER_DEV_DIR)]
    server_dev_dir: PathBuf,

    /// Whether to run version check
    #[env(GEL_RUN_VERSION_CHECK, EDGEDB_RUN_VERSION_CHECK)]
    run_version_check: VersionCheck,

    /// Path to pager executable
    #[env(GEL_PAGER, EDGEDB_PAGER)]
    pager: String,

    /// Debug flag for analyze JSON output
    #[env(_GEL_ANALYZE_DEBUG_JSON, _EDGEDB_ANALYZE_DEBUG_JSON)]
    _analyze_debug_json: bool,

    /// Debug flag for analyze plan output
    #[env(_GEL_ANALYZE_DEBUG_PLAN, _EDGEDB_ANALYZE_DEBUG_PLAN)]
    _analyze_debug_plan: bool,

    /// Cloud secret key
    #[env(GEL_CLOUD_SECRET_KEY, EDGEDB_CLOUD_SECRET_KEY)]
    cloud_secret_key: String,

    /// Cloud secret key
    #[env(GEL_SECRET_KEY, EDGEDB_SECRET_KEY)]
    secret_key: String,

    /// Cloud profile
    #[env(GEL_CLOUD_PROFILE, EDGEDB_CLOUD_PROFILE)]
    cloud_profile: String,

    /// Cloud certificates
    #[env(_GEL_CLOUD_CERTS, _EDGEDB_CLOUD_CERTS)]
    cloud_certs: CloudCerts,

    /// Cloud API endpoint URL
    #[env(GEL_CLOUD_API_ENDPOINT, EDGEDB_CLOUD_API_ENDPOINT)]
    cloud_api_endpoint: String,

    /// Skip WSL binary update
    #[env(_GEL_WSL_SKIP_UPDATE)]
    _wsl_skip_update: bool,

    /// WSL distro name
    #[env(_GEL_WSL_DISTRO, _EDGEDB_WSL_DISTRO)]
    _wsl_distro: String,

    /// Path to WSL Linux binary
    #[env(_GEL_WSL_LINUX_BINARY, _EDGEDB_WSL_LINUX_BINARY)]
    _wsl_linux_binary: PathBuf,

    /// Flag indicating Windows wrapper
    #[env(_GEL_FROM_WINDOWS, _EDGEDB_FROM_WINDOWS)]
    _from_windows: String,

    /// Package repository root URL
    #[env(GEL_PKG_ROOT, EDGEDB_PKG_ROOT)]
    pkg_root: String,

    /// System editor
    #[env(EDITOR)]
    system_editor: String,

    /// System pager
    #[env(PAGER)]
    system_pager: String,

    /// Skip any project hooks defined in gel.toml
    #[env(GEL_SKIP_HOOKS)]
    skip_hooks: BoolFlag,

    /// Whether we are running in a hook
    #[env(_GEL_IN_HOOK)]
    in_hook: BoolFlag,
}

pub fn get_envs(names: &[&str]) -> Result<Option<(String, String)>, anyhow::Error> {
    for name in names {
        let value = std::env::var(name).ok();
        if let Some(value) = value {
            return Ok(Some((name.to_string(), value)));
        }
    }
    Ok(None)
}

pub fn parse<T: std::str::FromStr>(s: &str) -> anyhow::Result<T>
where
    T::Err: std::fmt::Display,
{
    T::from_str(s).map_err(|e| anyhow::anyhow!("Invalid value: {e}"))
}

#[derive(Debug)]
pub enum VersionCheck {
    Never,
    Cached,
    Default,
    Strict,
}

impl std::str::FromStr for VersionCheck {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "never" => Ok(Self::Never),
            "cached" => Ok(Self::Cached),
            "default" => Ok(Self::Default),
            "strict" => Ok(Self::Strict),
            _ => Err(format!("Invalid value: {}", s)),
        }
    }
}

#[derive(Debug)]
pub enum InstallInDocker {
    Forbid,
    Allow,
    Default,
}

impl std::str::FromStr for InstallInDocker {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "forbid" => Ok(Self::Forbid),
            "allow" => Ok(Self::Allow),
            "default" => Ok(Self::Default),
            _ => Err(format!("Invalid value: {}", s)),
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct BoolFlag(pub bool);

impl std::str::FromStr for BoolFlag {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "true" | "1" => Ok(Self(true)),
            "false" | "0" => Ok(Self(false)),
            _ => Err(format!("Invalid boolean value: {}", s)),
        }
    }
}

impl std::ops::Deref for BoolFlag {
    type Target = bool;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
