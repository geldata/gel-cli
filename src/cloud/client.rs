use std::error::Error;
use std::fmt;
use std::fmt::Formatter;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use anyhow::Context;
use reqwest::{header, StatusCode};

use crate::branding::BRANDING_CLI_CMD;
use crate::cli::env::Env;
use crate::options::CloudOptions;
use crate::platform::config_dir;

const EDGEDB_CLOUD_DEFAULT_DNS_ZONE: &str = "aws.edgedb.cloud";
const EDGEDB_CLOUD_API_VERSION: &str = "v1/";
const EDGEDB_CLOUD_API_TIMEOUT: u64 = 10;
const REQUEST_RETRIES_COUNT: u32 = 10;
const REQUEST_RETRIES_MIN_INTERVAL: Duration = Duration::from_secs(1);
const REQUEST_RETRIES_MAX_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, serde::Deserialize, thiserror::Error)]
pub struct ErrorResponse {
    #[serde(skip, default)]
    pub code: StatusCode,
    status: String,
    error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("HTTP error: {0}")]
    ReqwestError(reqwest_middleware::Error),

    #[error(
        "HTTP permission error: {0}. This is usually caused by a firewall. Try disabling \
        your OS's firewall or any other firewalls you have installed"
    )]
    PermissionError(reqwest_middleware::Error),
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CloudConfig {
    pub secret_key: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct Claims {
    #[serde(rename = "iss", skip_serializing_if = "Option::is_none")]
    issuer: Option<String>,
}

pub struct CloudClient {
    client: reqwest_middleware::ClientWithMiddleware,
    pub is_logged_in: bool,
    pub api_endpoint: reqwest::Url,
    options_secret_key: Option<String>,
    options_profile: Option<String>,
    options_api_endpoint: Option<String>,
    pub secret_key: Option<String>,
    pub profile: Option<String>,
    pub is_default_partition: bool,
}

impl CloudClient {
    pub fn new(options: &CloudOptions) -> anyhow::Result<Self> {
        Self::new_inner(
            &options.cloud_secret_key,
            &options.cloud_profile,
            &options.cloud_api_endpoint,
        )
    }

    fn new_inner(
        options_secret_key: &Option<String>,
        options_profile: &Option<String>,
        options_api_endpoint: &Option<String>,
    ) -> anyhow::Result<Self> {
        let profile = if let Some(p) = options_profile.clone() {
            Some(p)
        } else {
            gel_tokio::env::Env::cloud_profile()?
        };
        let secret_key = if let Some(secret_key) = options_secret_key {
            Some(secret_key.into())
        } else if let Some(secret_key) = Env::cloud_secret_key()? {
            Some(secret_key)
        } else if let Some(secret_key) = gel_tokio::env::Env::secret_key()? {
            Some(secret_key)
        } else {
            match fs::read_to_string(cloud_config_file(&profile)?) {
                Ok(data) if data.is_empty() => None,
                Ok(data) => {
                    let config: CloudConfig = serde_json::from_str(&data)?;
                    config.secret_key
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => {
                    return Err(e)?;
                }
            }
        };
        let mut builder =
            reqwest::Client::builder().timeout(Duration::from_secs(EDGEDB_CLOUD_API_TIMEOUT));
        let is_logged_in;
        let dns_zone;
        if let Some(secret_key) = secret_key.clone() {
            let claims_b64 = secret_key
                .split('.')
                .nth(1)
                .context("malformed secret key: invalid JWT format")?;
            let claims = URL_SAFE_NO_PAD
                .decode(claims_b64)
                .context("malformed secret key: invalid base64 data")?;
            let claims: Claims = serde_json::from_slice(&claims)
                .context("malformed secret key: invalid JSON data")?;
            dns_zone = claims
                .issuer
                .context("malformed secret key: missing `iss` claim")?;

            let mut headers = header::HeaderMap::new();
            let auth_str = format!("Bearer {secret_key}");
            let mut auth_value = header::HeaderValue::from_str(&auth_str)?;
            auth_value.set_sensitive(true);
            headers.insert(header::AUTHORIZATION, auth_value.clone());
            // Duplicate the Authorization as X-Nebula-Authorization as
            // reqwest will strip the former on redirects.
            headers.insert("X-Nebula-Authorization", auth_value);

            let dns_zone2 = dns_zone.clone();
            let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > 5 {
                    attempt.error("too many redirects")
                } else {
                    match attempt.url().host_str() {
                        Some(host) if host.ends_with(&dns_zone2) => attempt.follow(),
                        // prevent redirects outside of the
                        // token issuer zone
                        Some(_) => attempt.stop(),
                        // relative redirect
                        None => attempt.follow(),
                    }
                }
            });

            builder = builder.default_headers(headers).redirect(redirect_policy);

            is_logged_in = true;
        } else {
            dns_zone = EDGEDB_CLOUD_DEFAULT_DNS_ZONE.to_string();
            is_logged_in = false;
        }
        let api_endpoint = if let Some(endpoint) = options_api_endpoint.clone() {
            endpoint
        } else if let Some(endpoint) = Env::cloud_api_endpoint()? {
            endpoint
        } else {
            format!("https://api.g.{dns_zone}")
        };

        let api_endpoint = reqwest::Url::parse(&api_endpoint)?;
        if let Some(cloud_certs) = gel_tokio::env::Env::_cloud_certs()? {
            log::info!("Using cloud certs for {cloud_certs:?}");
            let root = cloud_certs.root();
            log::trace!("{root}");
            // Add all certificates from the PEM bundle to the root store
            builder = builder
                .add_root_certificate(reqwest::Certificate::from_pem(root.as_bytes()).unwrap());
        }

        let retry_policy = reqwest_retry::policies::ExponentialBackoff::builder()
            .retry_bounds(REQUEST_RETRIES_MIN_INTERVAL, REQUEST_RETRIES_MAX_INTERVAL)
            .build_with_max_retries(REQUEST_RETRIES_COUNT);

        let retry_middleware =
            reqwest_retry::RetryTransientMiddleware::new_with_policy(retry_policy)
                .with_retry_log_level(tracing::Level::DEBUG);

        let client = reqwest_middleware::ClientBuilder::new(builder.build()?)
            .with(retry_middleware)
            .build();

        Ok(Self {
            client,
            is_logged_in,
            api_endpoint: api_endpoint.join(EDGEDB_CLOUD_API_VERSION)?,
            options_secret_key: options_secret_key.clone(),
            options_profile: options_profile.clone(),
            options_api_endpoint: options_api_endpoint.clone(),
            secret_key,
            profile,
            is_default_partition: (api_endpoint
                == reqwest::Url::parse(&format!("https://api.g.{EDGEDB_CLOUD_DEFAULT_DNS_ZONE}"))?),
        })
    }

    pub fn reinit(&mut self) -> anyhow::Result<()> {
        *self = Self::new_inner(
            &self.options_secret_key,
            &self.options_profile,
            &self.options_api_endpoint,
        )?;
        Ok(())
    }

    pub fn set_secret_key(&mut self, key: Option<&String>) -> anyhow::Result<()> {
        self.options_secret_key = key.cloned();
        self.reinit()
    }

    pub fn ensure_authenticated(&self) -> anyhow::Result<()> {
        if self.is_logged_in {
            Ok(())
        } else {
            anyhow::bail!("Run `{BRANDING_CLI_CMD} cloud login` first.")
        }
    }

    pub async fn request<T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest_middleware::RequestBuilder,
    ) -> anyhow::Result<T> {
        let resp = req.send().await.map_err(Self::create_error)?;
        if resp.status().is_success() {
            let full = resp.text().await?;
            serde_json::from_str(&full).with_context(|| {
                log::debug!("Response body: {}", full);
                "error decoding response body".to_string()
            })
        } else {
            let code = resp.status();
            let full = resp.text().await?;
            Err(anyhow::anyhow!(serde_json::from_str(&full)
                .map(|mut e: ErrorResponse| {
                    e.code = code;
                    e
                })
                .unwrap_or_else(|e| {
                    log::debug!("Response body: {}", full);
                    ErrorResponse {
                        code,
                        status: format!("error decoding response body: {e:#}"),
                        error: Some(full),
                    }
                })))
        }
    }

    fn create_error(err: reqwest_middleware::Error) -> HttpError {
        match err {
            reqwest_middleware::Error::Middleware(_) => HttpError::ReqwestError(err),

            reqwest_middleware::Error::Reqwest(ref reqwest_err) => {
                if let Some(io_error) = reqwest_err
                    .source()
                    .and_then(|v| v.source())
                    .and_then(|v| v.source())
                    .and_then(|v| v.downcast_ref::<io::Error>())
                    .and_then(|v| v.raw_os_error())
                {
                    // invalid permissions
                    if io_error == 1 {
                        return HttpError::PermissionError(err);
                    }
                }

                HttpError::ReqwestError(err)
            }
        }
    }

    pub async fn get<T: serde::de::DeserializeOwned>(
        &self,
        uri: impl AsRef<str>,
    ) -> anyhow::Result<T> {
        self.request(self.client.get(self.api_endpoint.join(uri.as_ref())?))
            .await
    }

    pub async fn post<T, J>(&self, uri: impl AsRef<str>, body: &J) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
        J: serde::Serialize + ?Sized,
    {
        self.request(
            self.client
                .post(self.api_endpoint.join(uri.as_ref())?)
                .json(body),
        )
        .await
    }

    pub async fn put<T, J>(&self, uri: impl AsRef<str>, body: &J) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
        J: serde::Serialize + ?Sized,
    {
        self.request(
            self.client
                .put(self.api_endpoint.join(uri.as_ref())?)
                .json(body),
        )
        .await
    }

    pub async fn delete<T: serde::de::DeserializeOwned>(
        &self,
        uri: impl AsRef<str>,
    ) -> anyhow::Result<T> {
        self.request(self.client.delete(self.api_endpoint.join(uri.as_ref())?))
            .await
    }
}

impl fmt::Display for ErrorResponse {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if let Some(error) = &self.error {
            write!(f, "{error}")
        } else {
            write!(f, "HTTP error: [{:?}] {}", self.code, self.status)
        }
    }
}

pub fn cloud_config_file(profile: &Option<String>) -> anyhow::Result<PathBuf> {
    Ok(cloud_config_dir()?.join(format!("{}.json", profile.as_deref().unwrap_or("default"))))
}

pub fn cloud_config_dir() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("cloud-credentials"))
}
