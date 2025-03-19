use std::net::SocketAddr;

use cmd::DockerProtocol;

use crate::{ProcessError, ProcessErrorType, ProcessRunner, SystemProcessRunner};

mod cmd;
mod instance;

#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("Docker is not installed")]
    NoDocker,
    #[error("Docker command failed: {0}")]
    ProcessError(#[from] ProcessError),
}

#[derive(Debug, PartialEq, Eq)]
pub struct GelDockerInstance {
    pub name: String,
    pub state: GelDockerInstanceState,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GelDockerInstanceState {
    Stopped,
    Running(GelDockerInstanceData),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct GelDockerInstanceData {
    pub cmdline: String,
    pub tls_key: Option<String>,
    pub tls_cert: Option<String>,
    pub jws_key: Option<String>,
    pub external_ports: Vec<SocketAddr>,
}

pub struct GelDockerInstances<P: ProcessRunner> {
    docker: cmd::DockerCommands<P>,
}

impl Default for GelDockerInstances<SystemProcessRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl GelDockerInstances<SystemProcessRunner> {
    pub fn new() -> Self {
        Self {
            docker: cmd::DockerCommands::new(SystemProcessRunner),
        }
    }
}

impl<P: ProcessRunner> GelDockerInstances<P> {
    pub fn new_with(runner: P) -> Self {
        Self {
            docker: cmd::DockerCommands::new(runner),
        }
    }

    /// Try to load from a `docker-compose.yaml` file.
    pub async fn try_load(&self) -> Result<Option<GelDockerInstance>, DockerError> {
        match self.try_load_inner().await {
            Ok(instance) => Ok(instance),
            Err(ProcessError {
                kind: ProcessErrorType::Io(e),
                ..
            }) if e.kind() == std::io::ErrorKind::NotFound => Err(DockerError::NoDocker),
            Err(e) => Err(DockerError::ProcessError(e)),
        }
    }

    /// Try to load from a given named container.
    pub async fn try_load_container(
        &self,
        name: &str,
    ) -> Result<Option<GelDockerInstance>, DockerError> {
        match self.try_load_container_inner(name).await {
            Ok(instance) => Ok(instance),
            Err(ProcessError {
                kind: ProcessErrorType::Io(e),
                ..
            }) if e.kind() == std::io::ErrorKind::NotFound => Err(DockerError::NoDocker),
            Err(e) => Err(DockerError::ProcessError(e)),
        }
    }

    async fn try_load_inner(&self) -> Result<Option<GelDockerInstance>, ProcessError> {
        let compose = self.docker.compose_ps().await?;
        let instance = compose.iter().find(|c| {
            c.has_label("com.docker.compose.oneoff=False")
                && (c.service == "gel" || c.service == "edgedb")
        });

        if let Some(instance) = instance {
            if instance.state != "running" {
                return Ok(Some(GelDockerInstance {
                    name: instance.name.clone(),
                    state: GelDockerInstanceState::Stopped,
                }));
            }

            self.try_load_container_inner(&instance.name).await
        } else {
            Ok(None)
        }
    }

    async fn try_load_container_inner(
        &self,
        name: &str,
    ) -> Result<Option<GelDockerInstance>, ProcessError> {
        let inspect = self.docker.inspect(name).await?;
        if let Some(inspect) = inspect.first() {
            if inspect.state.status != "running" {
                return Ok(Some(GelDockerInstance {
                    name: name.to_string(),
                    state: GelDockerInstanceState::Stopped,
                }));
            }
        } else {
            return Ok(Some(GelDockerInstance {
                name: name.to_string(),
                state: GelDockerInstanceState::Stopped,
            }));
        };

        let cmdline = self.docker.exec_cat(name, "/proc/1/cmdline").await?;
        log::debug!(
            "Docker command line: {}",
            cmdline.split('\0').collect::<Vec<_>>().join(" ")
        );
        struct Args {
            data_dir: Option<String>,
            tls_key_file: Option<String>,
            tls_cert_file: Option<String>,
            jws_key_file: Option<String>,
        }

        let mut args = Args {
            data_dir: None,
            tls_key_file: None,
            tls_cert_file: None,
            jws_key_file: None,
        };

        for segment in cmdline.split('\0') {
            for (arg, target) in [
                ("--tls-cert-file", &mut args.tls_cert_file),
                ("--tls-key-file", &mut args.tls_key_file),
                ("--jws-key-file", &mut args.jws_key_file),
                ("--data-dir", &mut args.data_dir),
            ] {
                if segment.starts_with(arg) {
                    if let Some(value) = segment.split('=').nth(1) {
                        *target = Some(value.to_string());
                    } else {
                        log::warn!("Invalid argument: {}", segment);
                    }
                }
            }
        }

        let mut instance_data = GelDockerInstanceData::default();

        if let Some(tls_key_file) = args.tls_key_file {
            let key_file = self.docker.exec_cat(name, tls_key_file).await?;
            instance_data.tls_key = Some(key_file);
        };
        if let Some(tls_cert_file) = args.tls_cert_file {
            let cert_file = self.docker.exec_cat(name, tls_cert_file).await?;
            instance_data.tls_cert = Some(cert_file);
        };

        if let Some(jws_key_file) = args.jws_key_file {
            let key_file = self.docker.exec_cat(name, jws_key_file).await?;
            instance_data.jws_key = Some(key_file);
        } else if let Some(data_dir) = args.data_dir {
            let key_file = self
                .docker
                .exec_cat(name, format!("{}/edbjwskeys.pem", data_dir).as_str())
                .await?;
            instance_data.jws_key = Some(key_file);
        }

        if let Some(inspect) = inspect.first() {
            let external_ports = inspect.port_mappings(5656, DockerProtocol::Tcp);
            instance_data.external_ports = external_ports;
        }

        Ok(Some(GelDockerInstance {
            name: name.to_string(),
            state: GelDockerInstanceState::Running(instance_data),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr},
        process::ExitStatus,
    };

    use rstest::{fixture, rstest};

    use super::*;
    use crate::MockProcessRunner;

    #[fixture]
    fn no_docker() -> MockProcessRunner {
        let mut runner = MockProcessRunner::default();
        runner.insert_err(
            "docker",
            ["compose", "ps", "-a", "--format", "json", "--no-trunc"],
            std::io::ErrorKind::NotFound,
        );
        runner
    }

    #[rstest]
    #[tokio::test]
    async fn test_no_docker(no_docker: MockProcessRunner) {
        let docker = GelDockerInstances::new_with(no_docker);
        let instance = docker.try_load().await.unwrap_err();
        assert!(matches!(instance, DockerError::NoDocker), "{instance:?}");
    }

    #[fixture]
    fn running() -> MockProcessRunner {
        let mut runner = MockProcessRunner::default();
        runner.insert_ok(
            "docker",
            ["compose", "ps", "-a", "--format", "json", "--no-trunc"],
            include_str!("../../testdata/docker/compose-ps.txt")
                .to_string()
                .into_bytes(),
            "".as_bytes(),
            ExitStatus::default(),
        );
        runner.insert_ok(
            "docker",
            ["inspect", "edgedb-cli-gel-1", "--format", "json"],
            include_str!("../../testdata/docker/inspect-compose.txt")
                .to_string()
                .into_bytes(),
            "".as_bytes(),
            ExitStatus::default(),
        );
        runner.insert_ok("docker", ["exec", "edgedb-cli-gel-1", "cat", "/proc/1/cmdline"], b"edgedb\0--data-dir=/var/lib/edgedb/data\0--tls-cert-file=/var/lib/edgedb/tls/cert.pem\0--tls-key-file=/var/lib/edgedb/tls/key.pem\0--jws-key-file=/var/lib/edgedb/data/edbjwskeys.pem\0", "".as_bytes(), ExitStatus::default());
        runner.insert_ok(
            "docker",
            [
                "exec",
                "edgedb-cli-gel-1",
                "cat",
                "/var/lib/edgedb/tls/cert.pem",
            ],
            include_str!("../../testdata/docker/tls-cert.pem")
                .to_string()
                .into_bytes(),
            "".as_bytes(),
            ExitStatus::default(),
        );
        runner.insert_ok(
            "docker",
            [
                "exec",
                "edgedb-cli-gel-1",
                "cat",
                "/var/lib/edgedb/tls/key.pem",
            ],
            include_str!("../../testdata/docker/tls-key.pem")
                .to_string()
                .into_bytes(),
            "".as_bytes(),
            ExitStatus::default(),
        );
        runner.insert_ok(
            "docker",
            [
                "exec",
                "edgedb-cli-gel-1",
                "cat",
                "/var/lib/edgedb/data/edbjwskeys.pem",
            ],
            include_str!("../../testdata/docker/jwk.pem")
                .to_string()
                .into_bytes(),
            "".as_bytes(),
            ExitStatus::default(),
        );
        runner
    }

    #[rstest]
    #[tokio::test]
    async fn test_try_load(running: MockProcessRunner) {
        let docker = GelDockerInstances::new_with(running);
        let instance = docker.try_load().await.unwrap().unwrap();

        let GelDockerInstanceState::Running(data) = instance.state else {
            panic!("Instance is not running");
        };

        assert!(
            data.tls_cert
                .as_ref()
                .unwrap()
                .contains("-----BEGIN CERTIFICATE-----"),
            "{}",
            data.tls_cert.as_ref().unwrap()
        );
        assert!(
            data.tls_key
                .as_ref()
                .unwrap()
                .contains("-----BEGIN RSA PRIVATE KEY-----"),
            "{}",
            data.tls_key.as_ref().unwrap()
        );
        assert!(
            data.jws_key
                .as_ref()
                .unwrap()
                .contains("-----BEGIN PRIVATE KEY-----"),
            "{}",
            data.jws_key.as_ref().unwrap()
        );
        assert_eq!(
            data.external_ports,
            vec![
                SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 5656),
                SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 5656),
            ]
        );
    }

    #[fixture]
    fn not_running() -> MockProcessRunner {
        let mut runner = MockProcessRunner::default();
        runner.insert_ok(
            "docker",
            ["compose", "ps", "-a", "--format", "json", "--no-trunc"],
            include_str!("../../testdata/docker/compose-ps-not-running.txt")
                .to_string()
                .into_bytes(),
            "".as_bytes(),
            ExitStatus::default(),
        );
        runner
    }

    #[rstest]
    #[tokio::test]
    async fn test_try_load_not_running(not_running: MockProcessRunner) {
        let docker = GelDockerInstances::new_with(not_running);
        let instance = docker.try_load().await.unwrap();
        assert_eq!(
            instance,
            Some(GelDockerInstance {
                name: "edgedb-cli-gel-1".to_string(),
                state: GelDockerInstanceState::Stopped,
            })
        );
    }
}
