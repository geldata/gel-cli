use std::{fmt::Debug, fs, io, path::PathBuf, time::Duration};

use gel_cli_instance::cloud::{CloudApi, CloudError, CloudHttp};
use gel_jwt::GelPublicKeyRegistry;
use gel_jwt::KeyRegistry;
use log::debug;
use log::warn;
use reqwest::header;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::branding::BRANDING_CLI_CMD;
use crate::cli::env::Env;
use crate::options::CloudOptions;
use crate::platform::config_dir;

const EDGEDB_CLOUD_DEFAULT_DNS_ZONE: &str = "aws.edgedb.cloud";
const EDGEDB_CLOUD_API_VERSION: &str = "v1/";
const EDGEDB_CLOUD_API_TIMEOUT: u64 = 10;
const REQUEST_RETRIES_COUNT: u32 = 10;
const REQUEST_RETRIES_MIN_INTERVAL: Duration = Duration::from_secs(1);
const REQUEST_RETRIES_MAX_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CloudConfig {
    pub secret_key: Option<String>,
}

pub struct Http {
    client: reqwest_middleware::ClientWithMiddleware,
}

impl Http {
    fn map_error(err: impl std::error::Error + Send + Sync + 'static) -> CloudError {
        let mut source_walker: &dyn std::error::Error = &err;
        while let Some(source) = source_walker.source() {
            if let Some(io_error) = source.downcast_ref::<std::io::Error>() {
                if io_error.raw_os_error() == Some(1) {
                    return CloudError::PermissionError;
                }
                return CloudError::OtherIo(io_error.kind(), io_error.to_string());
            }
            source_walker = source;
        }
        CloudError::CommunicationError(Box::new(err))
    }

    async fn map<T: DeserializeOwned + Debug>(
        resource: impl std::fmt::Display,
        req: reqwest_middleware::RequestBuilder,
    ) -> Result<T, CloudError> {
        let slow_task_warning = tokio::task::spawn(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            warn!("The server is taking a long time to respond.");
            tokio::time::sleep(Duration::from_secs(20)).await;
            warn!("The server is taking a very long time to respond.");
        });
        let resp = req.send().await;
        slow_task_warning.abort();
        let resp = resp.map_err(|e| Http::map_error(e))?;

        let status = resp.status();
        debug!("Got response: status: {:?}", status);
        if status.is_success() {
            let res = resp
                .json()
                .await
                .map_err(|e| CloudError::DeserializationError(Box::new(e)))?;
            debug!("Got response: body: {:?}", res);
            Ok(res)
        } else {
            #[derive(Debug, serde::Deserialize)]
            struct ErrorResponse {
                error: Option<String>,
            }

            debug!("Got error: status: {:?}", resp.status());
            let body = resp
                .text()
                .await
                .map_err(|e| CloudError::DeserializationError(Box::new(e)))?;
            debug!("Got error: body: {:?}", body);

            let message = if let Ok(body) = serde_json::from_str::<ErrorResponse>(&body) {
                body.error
            } else {
                if body.is_empty() { None } else { Some(body) }
            };

            match status {
                reqwest::StatusCode::BAD_REQUEST => {
                    Err(CloudError::BadRequest(message.unwrap_or_else(|| {
                        "The client sent a malformed request".to_string()
                    })))
                }
                reqwest::StatusCode::UNAUTHORIZED => Err(CloudError::Unauthorized),
                reqwest::StatusCode::NOT_FOUND => {
                    Err(CloudError::NotFound(message.unwrap_or_else(|| {
                        format!("The requested {} was not found", resource)
                    })))
                }
                _ => Err(CloudError::Other(
                    status.as_u16(),
                    message.unwrap_or_else(|| "An unknown error occurred".to_string()),
                )),
            }
        }
    }
}

impl CloudHttp for Http {
    async fn get<T: DeserializeOwned + Debug>(
        &self,
        resource: impl std::fmt::Display,
        url: &str,
    ) -> Result<T, CloudError> {
        debug!("GET {resource} {url}");
        Self::map(resource, self.client.get(url)).await
    }

    async fn post<REQ: Serialize + Debug, RES: DeserializeOwned + Debug>(
        &self,
        resource: impl std::fmt::Display,
        url: &str,
        body: REQ,
    ) -> Result<RES, CloudError> {
        debug!("POST {resource} {url} {body:?}");
        Self::map(resource, self.client.post(url).json(&body)).await
    }

    async fn put<REQ: Serialize + Debug, RES: DeserializeOwned + Debug>(
        &self,
        resource: impl std::fmt::Display,
        url: &str,
        body: REQ,
    ) -> Result<RES, CloudError> {
        debug!("PUT {resource} {url} {body:?}");
        Self::map(resource, self.client.put(url).json(&body)).await
    }

    async fn delete<T: DeserializeOwned + Debug>(
        &self,
        resource: impl std::fmt::Display,
        url: &str,
    ) -> Result<T, CloudError> {
        debug!("DELETE {resource} {url}");
        Self::map(resource, self.client.delete(url)).await
    }
}

pub struct CloudClient {
    pub api: CloudApi<Http>,
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
            Env::cloud_profile()?
        };
        let secret_key = if let Some(secret_key) = options_secret_key {
            Some(secret_key.into())
        } else if let Some(secret_key) = Env::cloud_secret_key()? {
            warn!(
                "Using deprecated cloud secret key from environment variable: Use GEL_SECRET_KEY instead"
            );
            Some(secret_key)
        } else if let Some(secret_key) = Env::secret_key()? {
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
            let claims = KeyRegistry::new().unsafely_decode_gel_token(&secret_key)?;
            dns_zone = claims
                .issuer
                .ok_or(anyhow::anyhow!("Missing issuer in secret key"))?;
            debug!("Issuer: {dns_zone}");

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
        if let Some(cloud_certs) = Env::cloud_certs()? {
            log::info!("Using cloud certs for {cloud_certs:?}");
            let root = cloud_certs.certificates_pem();
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

        let http = Http { client };

        Ok(Self {
            api: CloudApi::new(
                http,
                api_endpoint.join(EDGEDB_CLOUD_API_VERSION)?.to_string(),
            ),
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
}

pub fn cloud_config_file(profile: &Option<String>) -> anyhow::Result<PathBuf> {
    Ok(cloud_config_dir()?.join(format!("{}.json", profile.as_deref().unwrap_or("default"))))
}

pub fn cloud_config_dir() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("cloud-credentials"))
}
