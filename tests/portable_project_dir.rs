#![cfg(feature = "portable_tests")]

use assert_cmd::Command;
use predicates::prelude::*;

#[path = "common/util.rs"]
mod util;
use util::*;

#[test]
fn project_link_and_init_from_non_project_dir() {
    Command::new("gel")
        .arg("--version")
        .assert()
        .context("version", "command-line version option")
        .success()
        .stdout(predicates::str::contains(EXPECTED_VERSION));

    Command::new("gel")
        .arg("server")
        .arg("list-versions")
        .assert()
        .context("list-versions-before", "list with no installed")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("create")
        .arg("inst1")
        .assert()
        .context("proj-dir-create-1", "created `inst1`")
        .success();

    Command::new("gel")
        .arg("project")
        .arg("info")
        .arg("--instance-name")
        .arg("--project-dir=tests/proj/project1")
        .assert()
        .context("proj-dir-project-info-no", "not initialied")
        .code(1)
        .stderr(predicates::str::contains("is not initialized"));

    Command::new("gel")
        .arg("project")
        .arg("init")
        .arg("--link")
        .arg("--server-instance=inst1")
        .arg("--non-interactive")
        .arg("--project-dir=tests/proj/project1")
        .assert()
        .context(
            "proj-dir-project-link",
            "linked `inst1` to project project1",
        )
        .success();

    Command::new("gel")
        .arg("project")
        .arg("info")
        .arg("--instance-name")
        .arg("--project-dir=tests/proj/project1")
        .assert()
        .context("proj-dir-project-info", "instance-name == inst1")
        .success()
        .stdout(predicates::ord::eq("inst1\n"));

    Command::new("gel")
        .arg("query")
        .arg("SELECT 1")
        .current_dir("tests/proj/project1")
        .assert()
        .context("proj-dir-query-1", "query of project")
        .success();

    Command::new("gel")
        .arg("project")
        .arg("init")
        .arg("--non-interactive")
        .arg("--project-dir=tests/proj/project2")
        .assert()
        .context("proj-dir-project-init", "init project2")
        .success();

    Command::new("gel")
        .arg("project")
        .arg("info")
        .arg("--instance-name")
        .arg("--project-dir=tests/proj/project2")
        .assert()
        .context("proj-dir-project-info", "instance-name == project2")
        .success()
        .stdout(predicates::ord::eq("project2\n"));

    Command::new("gel")
        .arg("query")
        .arg("SELECT 1")
        .current_dir("tests/proj/project2")
        .assert()
        .context("proj-dir-query-2", "query of project2")
        .success();

    Command::new("gel")
        .arg("project")
        .arg("upgrade")
        .arg("--force")
        .arg("--project-dir=tests/proj/project2")
        .assert()
        .context("proj-dir-project-upgrade", "upgrade project")
        .success();

    Command::new("gel")
        .arg("query")
        .arg("SELECT 1")
        .current_dir("tests/proj/project2")
        .assert()
        .context("proj-dir-query-3", "query after upgrade")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("destroy")
        .arg("--instance=project2")
        .arg("--non-interactive")
        .assert()
        .context("proj-dir-destroy-2-no", "should warn")
        .code(2);

    Command::new("gel")
        .arg("instance")
        .arg("destroy")
        .arg("--instance=inst1")
        .arg("--non-interactive")
        .assert()
        .context("proj-dir-destroy-1-no", "should warn")
        .code(2);

    Command::new("gel")
        .arg("instance")
        .arg("destroy")
        .arg("--instance=project1")
        .arg("--non-interactive")
        .assert()
        .context(
            "proj-dir-destroy-1-non-exist",
            "it's project name, not instance name",
        )
        .code(8); // instance not found

    Command::new("gel")
        .arg("instance")
        .arg("list")
        .assert()
        .context("proj-dir-instance-list-1", "list two instances")
        .success()
        .stdout(predicates::str::contains("inst1"))
        .stdout(predicates::str::contains("project2"));

    Command::new("gel")
        .arg("instance")
        .arg("destroy")
        .arg("--instance=project2")
        .arg("--force")
        .assert()
        .context("proj-dir-destroy-2", "should destroy")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("list")
        .assert()
        .context("proj-dir-instance-list-2", "list once instance")
        .success()
        .stdout(predicates::str::contains("inst1"))
        .stdout(predicates::str::contains("project2").not());

    Command::new("gel")
        .arg("project")
        .arg("unlink")
        .arg("-D")
        .arg("--non-interactive")
        .arg("--project-dir=tests/proj/project1")
        .assert()
        .context("proj-dir-destroy-1", "should unlink and destroy project")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("list")
        .assert()
        .context("proj-dir-instance-list-3", "list no instances")
        .success()
        .stdout(predicates::str::contains("inst1").not())
        .stdout(predicates::str::contains("project2").not());

    Command::new("gel")
        .arg("project")
        .arg("init")
        .arg("--non-interactive")
        .arg("--project-dir=tests/proj/project2")
        .assert()
        .context("proj-dir-project-init-2", "init project2")
        .success();

    Command::new("gel")
        .arg("project")
        .arg("upgrade")
        .arg("--to-latest")
        .arg("--force")
        .arg("--project-dir=tests/proj/project2")
        .assert()
        .context("proj-dir-project-upgrade", "upgrade project2")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("status")
        .arg("--instance=project2")
        .arg("--extended")
        .assert()
        .context("proj-dir-instance-status", "show extended status")
        .success();

    Command::new("gel")
        .arg("instance")
        .arg("revert")
        .arg("--instance=project2")
        .arg("--no-confirm")
        .assert()
        .context("proj-dir-project-revert-2", "revert project2")
        .success();

    Command::new("gel")
        .arg("project")
        .arg("unlink")
        .arg("-D")
        .arg("--non-interactive")
        .arg("--project-dir=tests/proj/project2")
        .assert()
        .context("proj-dir-destroy-2", "should unlink and destroy project")
        .success();
}
