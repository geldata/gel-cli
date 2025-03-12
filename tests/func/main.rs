#[cfg(not(windows))]
#[macro_use]
extern crate pretty_assertions;

#[path = "../common/util.rs"]
mod util;
use util::*;

use std::fs;
use std::path::Path;
use std::process;
use std::str::FromStr;
use std::time::Duration;

use assert_cmd::Command;
use once_cell::sync::Lazy;
use test_utils::server::ServerInstance;

// Can't run server on windows
#[cfg(not(windows))]
mod configure;
#[cfg(not(windows))]
mod dump_restore;
#[cfg(not(windows))]
mod instance_link;
#[cfg(not(windows))]
mod migrations;
#[cfg(not(windows))]
mod non_interactive;

// for some reason rexpect doesn't work on macos
// and also something wrong on musl libc
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
mod interactive;

mod help;

pub const BRANDING_CLI_CMD: &str = if cfg!(feature = "gel") {
    "gel"
} else {
    "edgedb"
};

fn edgedb_cli_cmd() -> assert_cmd::Command {
    let mut cmd = Command::cargo_bin("gel").expect("binary found");
    cmd.timeout(Duration::from_secs(60));
    cmd.env("CLICOLOR", "0").arg("--no-cli-update-check");
    cmd
}

struct ServerGuard(ServerInstance);

static SERVER: Lazy<ServerGuard> = Lazy::new(start_server);

fn start_server() -> ServerGuard {
    shutdown_hooks::add_shutdown_hook(stop_server);

    ServerGuard(ServerInstance::start())
}

extern "C" fn stop_server() {
    SERVER.0.stop()
}

pub struct Config {
    dir: tempfile::TempDir,
}

impl Config {
    pub fn new(data: &str) -> Config {
        let tmp_dir = tempfile::tempdir().expect("tmpdir");
        let dir = tmp_dir.path().join("edgedb");
        fs::create_dir(&dir).expect("mkdir");
        fs::write(dir.join("cli.toml"), data.as_bytes()).expect("config");
        Config { dir: tmp_dir }
    }
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

#[cfg(not(windows))]
#[test]
fn simple_query() {
    let cmd = SERVER.admin_cmd().arg("query").arg("SELECT 1+7").assert();
    cmd.success().stdout("8\n");
}

#[cfg(not(windows))]
#[test]
fn version() {
    let cmd = SERVER.admin_cmd().arg("--version").assert();
    cmd.success()
        .stdout(predicates::str::contains(EXPECTED_VERSION));
}

impl ServerGuard {
    pub fn default_branch(&self) -> &'static str {
        if self.0.version_major.is_at_least(5) {
            "main"
        } else {
            "edgedb"
        }
    }

    pub fn admin_cmd(&self) -> Command {
        let mut cmd = edgedb_cli_cmd();
        cmd.arg("--admin");
        cmd.arg("--unix-path").arg(&self.0.info.socket_dir);
        cmd.arg("--port").arg(self.0.info.port.to_string());
        cmd.env("CLICOLOR", "0");
        cmd
    }

    #[cfg(not(windows))]
    pub fn admin_interactive(&self) -> rexpect::session::PtySession {
        use assert_cmd::cargo::CommandCargoExt;
        use rexpect::session::spawn_command;

        let mut cmd = process::Command::cargo_bin("gel").expect("binary found");
        cmd.arg("--no-cli-update-check");
        cmd.arg("--admin");
        cmd.arg("--unix-path").arg(&self.0.info.socket_dir);
        cmd.arg("--port").arg(self.0.info.port.to_string());
        spawn_command(cmd, Some(10000)).expect("start interactive")
    }
    #[cfg(not(windows))]
    pub fn custom_interactive(
        &self,
        f: impl FnOnce(&mut process::Command),
    ) -> rexpect::session::PtySession {
        use assert_cmd::cargo::CommandCargoExt;
        use rexpect::session::spawn_command;

        let mut cmd = process::Command::cargo_bin("gel").expect("binary found");
        cmd.arg("--no-cli-update-check");
        cmd.arg("--admin");
        cmd.arg("--unix-path").arg(&self.0.info.socket_dir);
        cmd.arg("--port").arg(self.0.info.port.to_string());
        cmd.arg("--tls-ca-file").arg(&self.0.info.tls_cert_file);
        cmd.env("CLICOLOR", "0");
        f(&mut cmd);
        spawn_command(cmd, Some(10000)).expect("start interactive")
    }

    pub fn database_cmd(&self, database_name: &str) -> Command {
        let mut cmd = self.admin_cmd();
        cmd.arg("--tls-ca-file").arg(&self.0.info.tls_cert_file);
        cmd.arg("--database").arg(database_name);
        cmd
    }

    pub fn ensure_instance_linked(&self) -> &'static str {
        const INSTANCE_NAME: &str = "_test_inst";
        edgedb_cli_cmd()
            .arg("instance")
            .arg("link")
            .arg("--port")
            .arg(self.0.info.port.to_string())
            .arg("--non-interactive")
            .arg("--trust-tls-cert")
            .arg("--overwrite")
            .arg("--quiet")
            .arg(INSTANCE_NAME)
            .assert()
            .success();

        INSTANCE_NAME
    }
}

/// Remove a migration file, without needing to know its hash in advance.
#[track_caller]
fn rm_migration_files(schema_dir: &str, migration_indexes: &[u16]) {
    let mut migrations_dir = std::path::PathBuf::from_str(schema_dir).unwrap();
    migrations_dir.push("migrations");

    let Ok(read_dir) = fs::read_dir(migrations_dir) else {
        return;
    };
    for entry in read_dir {
        let Ok(entry) = entry else {
            continue;
        };

        let file_name = entry.file_name().into_string().unwrap();
        let mig_index = file_name.split('-').next().unwrap();

        let mig_index: u16 = mig_index.parse().unwrap();
        if !migration_indexes.contains(&mig_index) {
            continue;
        }

        fs::remove_file(entry.path()).unwrap();
    }
}
