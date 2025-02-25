#![cfg(feature = "portable_tests")]

use assert_cmd::Command;
use predicates::prelude::*;

#[path = "common/util.rs"]
mod util;
use util::*;

#[test]
fn install() {
    Command::new("gel")
        .arg("--version")
        .assert()
        .context("version", "command-line version option")
        .success()
        .stdout(predicates::str::contains(EXPECTED_VERSION));

    // TODO(tailhook) check output somehow
    Command::new("gel")
        .arg("server")
        .arg("list-versions")
        .assert()
        .context("list-versions", "list versions of the server")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("create")
        .arg("inst1")
        .arg("my-branch")
        .assert()
        .context("create-1", "created `inst1`")
        .success();

    // TODO(tailhook) check output somehow
    Command::new("gel")
        .arg("server")
        .arg("info")
        .arg("--latest")
        .assert()
        .context("server-info", "show info about just installed server")
        .success();

    Command::new("gel")
        .arg("server")
        .arg("info")
        .arg("--get")
        .arg("bin-path")
        .arg("--latest")
        .assert()
        .context("server-info", "show binary parth")
        .success()
        .stdout(predicates::str::contains("edgedb-server"));

    // TODO check output somehow
    Command::new("gel")
        .arg("server")
        .arg("info")
        .arg("--get")
        .arg("version")
        .arg("--latest")
        .assert()
        .context("server-info", "show server version")
        .success();

    // TODO check output somehow
    Command::new("gel")
        .arg("server")
        .arg("info")
        .arg("--json")
        .arg("--get")
        .arg("version")
        .arg("--latest")
        .assert()
        .context("server-info", "show server version")
        .success();

    Command::new("gel")
        .arg("server")
        .arg("list-versions")
        .arg("--installed-only")
        .assert()
        .context("list-versions-installed", "")
        .success();

    Command::new("gel")
        .arg("server")
        .arg("list-versions")
        .arg("--json")
        .assert()
        .context("list-versions-json", "")
        .success();

    Command::new("gel")
        .arg("server")
        .arg("list-versions")
        .arg("--json")
        .arg("--installed-only")
        .assert()
        .context("list-versions-json-installed", "")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("logs")
        .arg("--instance=inst1")
        .assert()
        .context("log-1-0", "logs of `inst1`")
        .success();

    Command::new("gel")
        .arg("--instance")
        .arg("inst1")
        .arg("query")
        .arg("SELECT 1")
        .assert()
        .context("query-1", "query `inst1` first time")
        .success();

    Command::new("gel")
        .arg("--instance")
        .arg("inst1")
        .arg("query")
        .arg("--")
        .arg("select sys::get_current_branch();")
        .assert()
        .context("query-2", "query `inst1` to get current branch")
        .success()
        .stdout(predicates::str::contains("\"my-branch\""));

    Command::new("gel")
        .arg("instance")
        .arg("status")
        .arg("--instance=inst1")
        .assert()
        .context("status-1", "status `inst1` first time")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("restart")
        .arg("--instance=inst1")
        .assert()
        .context("restart-1", "restart `inst1`")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("logs")
        .arg("--instance=inst1")
        .assert()
        .context("log-1-1", "logs of `inst1`")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("status")
        .arg("--instance=inst1")
        .assert()
        .context("status-1-1", "status `inst1` after restart")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("stop")
        .arg("--instance=inst1")
        .env("RUST_LOG", "warn,edgedb::process=debug")
        .assert()
        .context("stop-1", "stop `inst1`")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("status")
        .arg("--instance=inst1")
        .assert()
        .context("status-1-2", "status `inst1` after stop")
        .code(3);

    Command::new("gel")
        .arg("instance")
        .arg("create")
        .arg("second")
        .arg("--nightly")
        // .arg("--version=1-beta3")  TODO(tailhook)
        .assert()
        .context("create-2", "create `second`")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("list")
        .assert()
        .context("instance-list-1", "list two instances")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("start")
        .arg("--instance=second")
        .assert()
        .context("start-2", "start `second`")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("start")
        .arg("--instance=inst1")
        .assert()
        .context("start-1-3", "start `inst1` again")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("status")
        .arg("--instance=second")
        .assert()
        .context("status-2", "status `second`")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("logs")
        .arg("--instance=inst1")
        .assert()
        .context("log-1-2", "logs of `inst1`")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("logs")
        .arg("--instance=second")
        .assert()
        .context("log-2", "logs of `second`")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("status")
        .arg("--instance=inst1")
        .assert()
        .context("status-1-4", "status of `inst1`")
        .success();

    // minor upgrade
    Command::new("gel")
        .arg("instance")
        .arg("upgrade")
        .arg("--instance=inst1")
        .arg("--force")
        .assert()
        .context("upgrade-1", "force upgrade `inst1` to latest")
        .success();

    Command::new("gel")
        .arg("--instance")
        .arg("inst1")
        .arg("query")
        .arg("SELECT 1")
        .assert()
        .context("query-1-2", "query `inst1` after upgrade")
        .success();

    // major upgrade
    Command::new("gel")
        .arg("instance")
        .arg("upgrade")
        .arg("--instance=inst1")
        .arg("--to-latest")
        .arg("--force")
        .assert()
        .context("upgrade-2", "force upgrade `inst1` to latest")
        .success();

    Command::new("gel")
        .arg("--instance")
        .arg("inst1")
        .arg("query")
        .arg("SELECT 1")
        .assert()
        .context("query-1-3", "query `inst1` after 2nd upgrade")
        .success();

    Command::new("gel")
        .arg("--instance=second")
        .arg("extension")
        .arg("list")
        .assert()
        .context("extension-list", "basic list of the installed extensions")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("destroy")
        .arg("second")
        .arg("--non-interactive")
        .assert()
        .context("destroy-2", "with a positional argument")
        .failure()
        .stderr(predicates::str::contains(
            "positional argument has been removed",
        ));

    Command::new("gel")
        .arg("instance")
        .arg("destroy")
        .arg("--instance=second")
        .arg("--non-interactive")
        .assert()
        .context("destroy-2", "destroy `second` instance")
        .success();

    Command::new("gel")
        .arg("server")
        .arg("uninstall")
        .arg("--unused")
        .assert()
        .context("uninstall-2", "uninstall old version")
        .success();

    Command::new("gel")
        .arg("server")
        .arg("list-versions")
        .arg("--installed-only")
        .arg("--column=major-version")
        .assert()
        .success()
        .context("list-2", "list after uninstall")
        .stdout(predicates::str::contains("-dev.").not());

    Command::new("gel")
        .arg("--instance")
        .arg("inst1")
        .arg("query")
        .arg("SELECT 1")
        .assert()
        .context("query-1a", "late query of `inst1`")
        .success();
}
