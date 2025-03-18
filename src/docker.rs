use std::{collections::HashMap, iter::FromIterator, net::SocketAddr};

use colorful::Colorful;
use gel_cli_instance::docker::{GelDockerInstance, GelDockerInstanceState, GelDockerInstances};
use gel_dsn::gel::TlsSecurity;
use gel_jwt::{KeyRegistry, PrivateKey, SigningContext};
use gel_tokio::Builder;
use log::warn;

pub enum DockerMode {
    ExplicitAuto,
    ExplicitInstance(String),
}

fn gel_instance_address_and_token(
    instance: GelDockerInstance,
) -> anyhow::Result<Option<(SocketAddr, String)>> {
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
            "found docker instance at {}, but the host could not be found",
            instance.name
        );
        return Ok(None);
    };
    let mut key_registry = KeyRegistry::<PrivateKey>::default();
    key_registry.add_from_any(&jws_key)?;
    let ctx = SigningContext::default();
    let token = key_registry.sign(
        HashMap::from_iter([("edgedb.server.any_role".to_string(), true.into())]),
        &ctx,
    )?;
    Ok(Some((*host, token)))
}

pub async fn try_docker(mut builder: Builder, mode: DockerMode) -> anyhow::Result<Builder> {
    let res = match mode {
        DockerMode::ExplicitAuto => GelDockerInstances::new().try_load().await,
        DockerMode::ExplicitInstance(name) => {
            GelDockerInstances::new().try_load_container(&name).await
        }
    }?;
    if let Some(instance) = res {
        if let Some((host, token)) = gel_instance_address_and_token(instance)? {
            builder = builder
                .secret_key(format!("edbt_{token}"))
                .host(host.ip())
                .port(host.port())
                .tls_security(TlsSecurity::Insecure);
        }
    }

    Ok(builder)
}

pub async fn try_docker_fallback(mut builder: Builder) -> anyhow::Result<Builder> {
    if let Some(instance) = GelDockerInstances::new().try_load().await? {
        let name = instance.name.clone();
        if let Some((host, token)) = gel_instance_address_and_token(instance)? {
            eprintln!("Using docker container named {} at {host:#}", name.bold());

            builder = builder
                .secret_key(format!("edbt_{token}"))
                .host(host.ip())
                .port(host.port())
                .tls_security(TlsSecurity::Insecure);
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

#[tokio::main]
pub async fn has_docker_blocking() -> bool {
    has_docker().await
}
