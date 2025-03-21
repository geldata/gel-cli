use humantime_serde::re::humantime;
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::HashMap, fmt::Debug};

mod schema;
pub use schema::*;

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error(
        "Permission error while attempting to make an HTTP request. This may be due to a firewall blocking the request."
    )]
    PermissionError,
    #[error("I/O error while attempting to make an HTTP request: {0} {1}")]
    OtherIo(std::io::ErrorKind, String),
    #[error("Unauthorized")]
    Unauthorized,
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Bad request: {0}")]
    BadRequest(String),
    #[error("HTTP error {0}: {1}")]
    Other(u16, String),
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Communication error: {0}")]
    CommunicationError(Box<dyn std::error::Error + Send + Sync>),
    #[error("Deserialization error: {0}")]
    DeserializationError(Box<dyn std::error::Error + Send + Sync>),
}

#[allow(async_fn_in_trait)]
pub trait CloudHttp {
    async fn get<T: DeserializeOwned + Debug>(
        &self,
        what: impl std::fmt::Display,
        url: &str,
    ) -> Result<T, CloudError>;
    async fn post<REQ: Serialize + Debug, RES: DeserializeOwned + Debug>(
        &self,
        what: impl std::fmt::Display,
        url: &str,
        body: REQ,
    ) -> Result<RES, CloudError>;
    async fn put<REQ: Serialize + Debug, RES: DeserializeOwned + Debug>(
        &self,
        what: impl std::fmt::Display,
        url: &str,
        body: REQ,
    ) -> Result<RES, CloudError>;
    async fn delete<T: DeserializeOwned + Debug>(
        &self,
        what: impl std::fmt::Display,
        url: &str,
    ) -> Result<T, CloudError>;
}

pub struct CloudApi<H: CloudHttp> {
    http: H,
    endpoint: String,
}

impl<H: CloudHttp> CloudApi<H> {
    pub fn new(http: H, endpoint: String) -> Self {
        Self { http, endpoint }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.endpoint, path)
    }
}

#[derive(Debug, Clone, derive_more::Display)]
enum CloudResource<'a> {
    #[display("current user")]
    User,
    #[display("user session with id '{_0}'")]
    UserSession(&'a str),
    #[display("user sessions")]
    UserSessions,
    #[display("cloud operation with id '{_0}'")]
    CloudOperation(&'a str),
    #[display("instances")]
    Instances,
    #[display("instance with name '{_0}/{_1}'")]
    Instance(&'a str, &'a str),
    #[display("organization with name '{_0}'")]
    Org(&'a str),
    #[display("secret keys")]
    SecretKeys,
    #[display("secret key with id '{_0}'")]
    SecretKey(&'a str),
    #[display("versions")]
    Versions,
    #[display("pricing")]
    Pricing,
    #[display("region")]
    Region,
}

impl<H: CloudHttp> CloudApi<H> {
    pub async fn get_user(&self) -> Result<schema::User, CloudError> {
        self.http
            .get(CloudResource::User, &self.endpoint("user"))
            .await
    }

    pub async fn create_session(
        &self,
        session_type: &str,
    ) -> Result<schema::UserSessionCreated, CloudError> {
        // TODO: API mismatch?
        let body = HashMap::from([("type", session_type)]);
        self.http
            .post(
                CloudResource::UserSessions,
                &self.endpoint("auth/sessions"),
                body,
            )
            .await
    }

    pub async fn get_session(&self, session_id: &str) -> Result<schema::UserSession, CloudError> {
        self.http
            .get(
                CloudResource::UserSession(session_id),
                &self.endpoint(&format!("auth/sessions/{}", session_id)),
            )
            .await
    }

    pub async fn get_operation(
        &self,
        operation_id: &str,
    ) -> Result<schema::CloudOperation, CloudError> {
        self.http
            .get(
                CloudResource::CloudOperation(operation_id),
                &self.endpoint(&format!("operations/{}", operation_id)),
            )
            .await
    }

    pub async fn list_instances(&self) -> Result<Vec<schema::CloudInstance>, CloudError> {
        self.http
            .get(CloudResource::Instances, &self.endpoint("instances"))
            .await
    }

    pub async fn get_instance(
        &self,
        org_slug: &str,
        name: &str,
    ) -> Result<schema::CloudInstance, CloudError> {
        self.http
            .get(
                CloudResource::Instance(org_slug, name),
                &self.endpoint(&format!("orgs/{org_slug}/instances/{name}")),
            )
            .await
    }

    pub async fn delete_instance(
        &self,
        org_slug: &str,
        name: &str,
    ) -> Result<schema::CloudOperation, CloudError> {
        self.http
            .delete(
                CloudResource::Instance(org_slug, name),
                &self.endpoint(&format!("orgs/{org_slug}/instances/{name}")),
            )
            .await
    }

    pub async fn create_instance(
        &self,
        org_slug: &str,
        request: schema::CloudInstanceCreate,
    ) -> Result<schema::CloudOperation, CloudError> {
        self.http
            .post(
                CloudResource::Instance(org_slug, &request.name.to_owned()),
                &self.endpoint(&format!("orgs/{org_slug}/instances")),
                request,
            )
            .await
    }

    pub async fn upgrade_instance(
        &self,
        org_slug: &str,
        name: &str,
        request: schema::CloudInstanceUpgrade,
    ) -> Result<schema::CloudOperation, CloudError> {
        self.http
            .put(
                CloudResource::Instance(org_slug, name),
                &self.endpoint(&format!("orgs/{org_slug}/instances/{name}")),
                request,
            )
            .await
    }

    pub async fn resize_instance(
        &self,
        org_slug: &str,
        name: &str,
        request: schema::CloudInstanceResize,
    ) -> Result<schema::CloudOperation, CloudError> {
        self.http
            .put(
                CloudResource::Instance(org_slug, name),
                &self.endpoint(&format!("orgs/{org_slug}/instances/{name}")),
                request,
            )
            .await
    }

    pub async fn get_org(&self, org_slug: &str) -> Result<schema::Org, CloudError> {
        self.http
            .get(
                CloudResource::Org(org_slug),
                &self.endpoint(&format!("orgs/{org_slug}")),
            )
            .await
    }

    pub async fn create_backup(
        &self,
        org_slug: &str,
        name: &str,
    ) -> Result<schema::CloudOperation, CloudError> {
        // TODO: Missing API in doc
        self.http
            .post(
                CloudResource::Instance(org_slug, name),
                &self.endpoint(&format!("orgs/{org_slug}/instances/{name}/backups")),
                {},
            )
            .await
    }

    pub async fn list_backups(
        &self,
        org_slug: &str,
        name: &str,
    ) -> Result<Vec<schema::Backup>, CloudError> {
        self.http
            .get(
                CloudResource::Instance(org_slug, name),
                &self.endpoint(&format!("orgs/{org_slug}/instances/{name}/backups")),
            )
            .await
    }

    pub async fn restore_instance(
        &self,
        org_slug: &str,
        name: &str,
        request: schema::CloudInstanceRestore,
    ) -> Result<schema::CloudOperation, CloudError> {
        self.http
            .post(
                CloudResource::Instance(org_slug, name),
                &self.endpoint(&format!("orgs/{org_slug}/instances/{name}/restore")),
                request,
            )
            .await
    }

    pub async fn restart_instance(
        &self,
        org_slug: &str,
        name: &str,
    ) -> Result<schema::CloudOperation, CloudError> {
        self.http
            .post(
                CloudResource::Instance(org_slug, name),
                &self.endpoint(&format!("orgs/{org_slug}/instances/{name}/restart")),
                {},
            )
            .await
    }

    pub async fn get_instance_logs(
        &self,
        org_slug: &str,
        name: &str,
        limit: Option<usize>,
        start: Option<std::time::SystemTime>,
        to: Option<std::time::SystemTime>,
        direction: Option<&str>,
    ) -> Result<schema::Logs, CloudError> {
        let mut query_params = Vec::new();
        if let Some(limit) = limit {
            query_params.push(format!("limit={}", limit));
        }
        if let Some(start) = start {
            query_params.push(format!("start={}", humantime::format_rfc3339(start)));
        }
        if let Some(to) = to {
            query_params.push(format!("to={}", humantime::format_rfc3339(to)));
        }
        if let Some(direction) = direction {
            query_params.push(format!("direction={}", direction));
        }
        self.http
            .get(
                CloudResource::Instance(org_slug, name),
                &self.endpoint(&format!(
                    "orgs/{org_slug}/instances/{name}/logs?{}",
                    query_params.join("&")
                )),
            )
            .await
    }

    pub async fn create_secret_key(
        &self,
        input: schema::CreateSecretKeyInput,
    ) -> Result<schema::SecretKey, CloudError> {
        self.http
            .post(
                CloudResource::SecretKeys,
                &self.endpoint("secretkeys/"),
                input,
            )
            .await
    }

    pub async fn list_secret_keys(&self) -> Result<Vec<schema::SecretKey>, CloudError> {
        self.http
            .get(CloudResource::SecretKeys, &self.endpoint("secretkeys/"))
            .await
    }

    pub async fn delete_secret_key(
        &self,
        secret_key_id: &str,
    ) -> Result<schema::SecretKey, CloudError> {
        self.http
            .delete(
                CloudResource::SecretKey(secret_key_id),
                &self.endpoint(&format!("secretkeys/{}", secret_key_id)),
            )
            .await
    }

    pub async fn get_versions(&self) -> Result<Vec<schema::Version>, CloudError> {
        self.http
            .get(CloudResource::Versions, &self.endpoint("versions"))
            .await
    }

    pub async fn get_prices(&self) -> Result<schema::PricesResponse, CloudError> {
        self.http
            .get(CloudResource::Pricing, &self.endpoint("pricing"))
            .await
    }

    pub async fn get_current_region(&self) -> Result<schema::Region, CloudError> {
        self.http
            .get(CloudResource::Region, &self.endpoint("region/self"))
            .await
    }
}
