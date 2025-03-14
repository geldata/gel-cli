use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    process::Command,
};

use serde::{Deserialize, Serialize};

use crate::{ProcessError, ProcessRunner, Processes};

#[derive(Debug, Clone, Copy, derive_more::Display, Serialize, Deserialize)]
pub enum DockerProtocol {
    #[display("TCP")]
    Tcp,
    #[display("UDP")]
    Udp,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DockerComposeInstance {
    #[serde(rename = "Command")]
    pub command: String,
    #[serde(rename = "CreatedAt")]
    pub created_at: String,
    #[serde(rename = "ExitCode")]
    pub exit_code: i32,
    #[serde(rename = "Health")]
    pub health: String,
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Image")]
    pub image: String,
    #[serde(rename = "Labels")]
    pub labels: String,
    #[serde(rename = "LocalVolumes")]
    pub local_volumes: String,
    #[serde(rename = "Mounts")]
    pub mounts: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Names")]
    pub names: String,
    #[serde(rename = "Networks")]
    pub networks: String,
    #[serde(rename = "Ports")]
    pub ports: String,
    #[serde(rename = "Project")]
    pub project: String,
    #[serde(rename = "Publishers")]
    pub publishers: Vec<serde_json::Value>,
    #[serde(rename = "RunningFor")]
    pub running_for: String,
    #[serde(rename = "Service")]
    pub service: String,
    #[serde(rename = "Size")]
    pub size: String,
    #[serde(rename = "State")]
    pub state: String,
    #[serde(rename = "Status")]
    pub status: String,
}

impl DockerComposeInstance {
    pub fn has_label(&self, label: &str) -> bool {
        self.labels.contains(label)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DockerInspect {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Created")]
    pub created: String,
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "State")]
    pub state: DockerInspectState,
    #[serde(rename = "HostConfig")]
    pub host_config: DockerInspectHostConfig,
    #[serde(rename = "NetworkSettings")]
    pub network_settings: DockerInspectNetworkSettings,
    // Catch-all for other fields
    #[serde(flatten)]
    pub rest: HashMap<String, serde_json::Value>,
}

impl DockerInspect {
    /// Performs a best-effort mapping of the given port to the host.
    ///
    /// This function will return an empty vector if the port is not found or
    /// if the port is not mapped to the host.
    pub fn port_mappings(&self, port: u16, protocol: DockerProtocol) -> Vec<SocketAddr> {
        let mut result = Vec::new();
        let key = format!("{}/{}", port, protocol.to_string().to_lowercase());

        let ports = &self.network_settings.ports;
        if let Some(mappings) = ports.get(&key) {
            if let Some(mappings) = mappings.as_array() {
                for mapping in mappings {
                    let host_ip = mapping.get("HostIp").and_then(|v| v.as_str());
                    let host_port = mapping.get("HostPort").and_then(|v| v.as_str());
                    if let (Some(mut ip), Some(port)) = (host_ip, host_port) {
                        if ip.is_empty() {
                            ip = "0.0.0.0";
                        }
                        if let (Ok(ip), Ok(port)) = (ip.parse::<IpAddr>(), port.parse::<u16>()) {
                            result.push(SocketAddr::new(ip, port));
                        } else {
                            log::debug!(
                                "Invalid port mapping for {}: ip={} port={}",
                                key,
                                ip,
                                port
                            );
                        }
                    }
                }
            }
        } else {
            log::debug!(
                "No port mappings found for {}. Mappings were: {:?}",
                key,
                ports
            );
        }

        result
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DockerInspectState {
    #[serde(rename = "Status")]
    pub status: String,
    #[serde(rename = "Running")]
    pub running: bool,
    #[serde(rename = "Paused")]
    pub paused: bool,
    #[serde(rename = "Restarting")]
    pub restarting: bool,
    #[serde(rename = "OOMKilled")]
    pub oom_killed: bool,
    #[serde(rename = "Dead")]
    pub dead: bool,
    #[serde(rename = "Pid")]
    pub pid: i32,
    #[serde(rename = "ExitCode")]
    pub exit_code: i32,
    #[serde(rename = "Error")]
    pub error: String,
    #[serde(rename = "StartedAt")]
    pub started_at: String,
    #[serde(rename = "FinishedAt")]
    pub finished_at: String,

    #[serde(flatten)]
    pub rest: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DockerInspectHostConfig {
    #[serde(rename = "NetworkMode")]
    pub network_mode: String,
    #[serde(rename = "PortBindings")]
    pub port_bindings: HashMap<String, serde_json::Value>,

    #[serde(flatten)]
    pub rest: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DockerInspectNetworkSettings {
    #[serde(rename = "Ports")]
    pub ports: HashMap<String, serde_json::Value>,

    #[serde(flatten)]
    pub rest: HashMap<String, serde_json::Value>,
}

pub struct DockerCommands<P: ProcessRunner> {
    runner: Processes<P>,
}

impl<P: ProcessRunner> DockerCommands<P> {
    pub fn new(runner: P) -> Self {
        Self {
            runner: Processes::new(runner),
        }
    }

    pub async fn compose_ps(&self) -> Result<Vec<DockerComposeInstance>, ProcessError> {
        let mut cmd = Command::new("docker");
        cmd.args(["compose", "ps", "-a", "--format", "json", "--no-trunc"]);
        self.runner.run_json_slurp(cmd).await
    }

    #[allow(unused)]
    pub async fn info(&self) -> Result<serde_json::Value, ProcessError> {
        let mut cmd = Command::new("docker");
        cmd.args(["info", "--format", "json"]);
        self.runner.run_json(cmd).await
    }

    pub async fn inspect(&self, name: &str) -> Result<Vec<DockerInspect>, ProcessError> {
        let mut cmd = Command::new("docker");
        cmd.args(["inspect", name, "--format", "json"]);
        self.runner.run_json_slurp(cmd).await
    }

    #[allow(unused)]
    pub async fn exec(&self, name: &str, args: &[&str]) -> Result<String, ProcessError> {
        let mut cmd = Command::new("docker");
        cmd.args(["exec", name]);
        cmd.args(args);
        self.runner.run_string(cmd).await
    }

    pub async fn exec_cat(
        &self,
        name: &str,
        path: impl AsRef<str>,
    ) -> Result<String, ProcessError> {
        let mut cmd = Command::new("docker");
        cmd.args(["exec", name, "cat"]);
        cmd.arg(path.as_ref());
        self.runner.run_string(cmd).await
    }
}

#[cfg(test)]
mod tests {
    use crate::MockProcessRunner;
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr},
        process::ExitStatus,
    };

    use super::*;
    use rstest::{fixture, rstest};

    #[fixture]
    fn runner() -> MockProcessRunner {
        let mut runner = MockProcessRunner::default();
        runner.insert_ok(
            "docker",
            ["compose", "ps", "-a", "--format", "json", "--no-trunc"],
            include_str!("../../../testdata/docker/compose-ps.txt")
                .to_string()
                .into_bytes(),
            "".as_bytes(),
            ExitStatus::default(),
        );
        runner.insert_ok(
            "docker",
            ["inspect", "id-manual", "--format", "json"],
            include_str!("../../../testdata/docker/inspect-manual.txt")
                .to_string()
                .into_bytes(),
            "".as_bytes(),
            ExitStatus::default(),
        );
        runner.insert_ok(
            "docker",
            ["inspect", "id-compose", "--format", "json"],
            include_str!("../../../testdata/docker/inspect-compose.txt")
                .to_string()
                .into_bytes(),
            "".as_bytes(),
            ExitStatus::default(),
        );
        runner.insert_ok(
            "docker",
            ["inspect", "id-no-ports", "--format", "json"],
            include_str!("../../../testdata/docker/inspect-no-ports.txt")
                .to_string()
                .into_bytes(),
            "".as_bytes(),
            ExitStatus::default(),
        );
        runner
    }

    #[rstest]
    #[tokio::test]
    async fn test_list_compose(runner: MockProcessRunner) {
        let instance = DockerCommands::new(runner);
        let compose = instance.compose_ps().await.unwrap();
        for c in compose {
            if c.has_label("com.docker.compose.oneoff=False") {
                eprintln!("{:?}", c);
            }
        }
    }

    #[rstest]
    #[case::compose("id-compose", vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 5656), SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 5656)])]
    #[case::manual("id-manual", vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1234), SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 1234)])]
    #[case::no_ports("id-no-ports", vec![])]
    #[tokio::test]
    async fn test_inspect(
        #[case] name: &'static str,
        #[case] expected: Vec<SocketAddr>,
        runner: MockProcessRunner,
    ) {
        let instance = DockerCommands::new(runner);
        let inspect = instance.inspect(name).await.unwrap();

        let port_mappings = inspect[0].port_mappings(5656, DockerProtocol::Tcp);
        assert_eq!(port_mappings, expected);
    }
}
