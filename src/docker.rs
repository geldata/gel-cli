use std::net::SocketAddr;

use colorful::Colorful;
use gel_cli_instance::docker::{GelDockerInstance, GelDockerInstanceState, GelDockerInstances};
use gel_jwt::{GelPrivateKeyRegistry, Key, KeyRegistry, SigningContext};
use gel_tokio::Builder;
use gel_tokio::dsn::{HostType, TlsSecurity};
use log::warn;

pub enum DockerMode {
    ExplicitAuto,
    ExplicitInstance(String),
}

struct DockerInstanceData {
    host: HostType,
    port: u16,
    secret_key: String,
    tls_security: TlsSecurity,
    tls_ca: Option<String>,
}

fn gel_instance_address_and_token(
    instance: GelDockerInstance,
) -> anyhow::Result<Option<DockerInstanceData>> {
    let GelDockerInstanceState::Running(state) = instance.state else {
        warn!(
            "found docker instance at {}, but it is not running",
            instance.name
        );
        return Ok(None);
    };
    let Some(jws_key) = state.jws_key else {
        warn!(
            "found docker instance at {}, but the JWS key could not be found",
            instance.name
        );
        return Ok(None);
    };
    let Some(host) = state.external_ports.first() else {
        warn!(
            "found docker instance at {}, but the host and port to connect to could not be found. Is the port exposed?",
            instance.name
        );
        return Ok(None);
    };
    let mut key_registry = KeyRegistry::<Key>::default();
    key_registry.add_from_any(&jws_key)?;
    let ctx = SigningContext::default();
    let token = key_registry.generate_legacy_token(None, &ctx)?;

    let port = host.port();
    let host = match host {
        SocketAddr::V4(addr) if addr.ip().is_loopback() || addr.ip().is_unspecified() => {
            HostType::try_from_str("localhost").unwrap()
        }
        SocketAddr::V6(addr) if addr.ip().is_loopback() || addr.ip().is_unspecified() => {
            HostType::try_from_str("localhost").unwrap()
        }
        _ => host.ip().into(),
    };

    if let Some(tls_cert) = state.tls_cert {
        Ok(Some(DockerInstanceData {
            host,
            port,
            secret_key: format!("edbt_{token}"),
            tls_security: TlsSecurity::Default,
            tls_ca: Some(tls_cert),
        }))
    } else {
        Ok(Some(DockerInstanceData {
            host,
            port,
            secret_key: format!("edbt_{token}"),
            tls_security: TlsSecurity::Insecure,
            tls_ca: None,
        }))
    }
}

/// Create a builder from a host and token. If the host is a loopback or unspecified address,
/// the builder will be configured to connect to localhost.
fn builder_from_host_and_token(builder: Builder, data: DockerInstanceData) -> Builder {
    let mut builder = builder
        .secret_key(data.secret_key)
        .host(data.host)
        .port(data.port)
        .tls_security(data.tls_security);

    if let Some(tls_ca) = data.tls_ca {
        builder = builder.tls_ca_string(&tls_ca);
    }

    builder
}

pub async fn try_docker(mut builder: Builder, mode: DockerMode) -> anyhow::Result<Builder> {
    let res = match mode {
        DockerMode::ExplicitAuto => GelDockerInstances::new().try_load().await,
        DockerMode::ExplicitInstance(name) => {
            GelDockerInstances::new().try_load_container(&name).await
        }
    }?;
    if let Some(instance) = res {
        if let Some(data) = gel_instance_address_and_token(instance)? {
            builder = builder_from_host_and_token(builder, data);
        }
    }

    Ok(builder)
}

pub async fn try_docker_fallback(mut builder: Builder) -> anyhow::Result<Builder> {
    if let Some(instance) = GelDockerInstances::new().try_load().await? {
        let name = instance.name.clone();
        if let Some(data) = gel_instance_address_and_token(instance)? {
            eprintln!(
                "Using docker container named {} at {:#}",
                name.bold(),
                data.host
            );
            builder = builder_from_host_and_token(builder, data);
        }
    }
    Ok(builder)
}

pub async fn has_docker() -> bool {
    if let Ok(Some(instance)) = GelDockerInstances::new().try_load().await {
        if matches!(instance.state, GelDockerInstanceState::Running(_)) {
            return true;
        }
    }
    false
}
