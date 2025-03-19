use assert_cmd::Command;
use assert_cmd::assert::IntoOutputPredicate;
use predicates::boolean::PredicateBooleanExt;

use crate::SERVER;
use crate::util::OutputExt;

#[test]
fn with_comment() {
    SERVER
        .admin_cmd()
        .write_stdin("SELECT 1; # comment")
        .assert()
        .success();
}

#[test]
fn stdin_password() {
    SERVER
        .admin_cmd()
        .arg("--password-from-stdin")
        .write_stdin("password\n")
        .assert()
        .success();
}

#[test]
fn strict_version_check() {
    Command::cargo_bin("gel")
        .expect("binary found")
        .env("EDGEDB_RUN_VERSION_CHECK", "strict")
        .arg("info")
        .assert()
        .success();
}

#[test]
fn list_indexes() {
    SERVER
        .admin_cmd()
        .arg("list")
        .arg("indexes")
        .assert()
        .success();
}

#[test]
fn database_create_wipe_drop() {
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("test_create_wipe_drop")
        .assert()
        .context("create", "create new database")
        .success();

    SERVER
        .admin_cmd()
        .arg("query")
        .arg("--database=test_create_wipe_drop")
        .arg("CREATE TYPE Type1")
        .arg("INSERT Type1")
        .assert()
        .context("add-data", "add some data to the new database")
        .success();

    SERVER
        .admin_cmd()
        .arg("query")
        .arg("--database=test_create_wipe_drop")
        .arg("SELECT Type1")
        .assert()
        .context("check-data", "check that added data is still there")
        .stdout(predicates::str::contains(r#"{"id":"#))
        .success();

    SERVER
        .admin_cmd()
        .arg("database")
        .arg("wipe")
        .arg("--database=test_create_wipe_drop")
        .arg("--non-interactive")
        .assert()
        .context("wipe", "wipe the data out")
        .success();

    SERVER
        .admin_cmd()
        .arg("query")
        .arg("--database=test_create_wipe_drop")
        .arg("CREATE TYPE Type1")
        .assert()
        .context("create-again", "check that type can be created again")
        .success();

    SERVER
        .admin_cmd()
        .arg("--database=test_create_wipe_drop")
        .arg("database")
        .arg("drop")
        .arg("test_create_wipe_drop")
        .arg("--non-interactive")
        .assert()
        .context("drop-same", "cannot drop the same database")
        .failure();

    SERVER
        .admin_cmd()
        .arg("database")
        .arg("drop")
        .arg("test_create_wipe_drop")
        .arg("--non-interactive")
        .assert()
        .context("drop", "drop successfully")
        .success();

    SERVER
        .admin_cmd()
        .arg("query")
        .arg("--database=test_create_wipe_drop")
        .arg("SELECT Type1")
        .assert()
        .context("select-again", "make sure that database is not there")
        .failure();
}

#[test]
fn branch_commands() {
    let default_branch = SERVER.default_branch();

    SERVER
        .admin_cmd()
        .arg("branch")
        .arg("current")
        .arg("--plain")
        .assert()
        .context("current", "should print the default branch")
        .success()
        .stdout(predicates::str::contains(default_branch));

    // create --empty
    SERVER
        .admin_cmd()
        .arg("branch")
        .arg("create")
        .arg("--empty")
        .arg("test_branch_1")
        .assert()
        .context("create", "new empty branch")
        .success();

    // create --from
    SERVER
        .admin_cmd()
        .arg("branch")
        .arg("create")
        .arg("test_branch_2")
        .arg("--from")
        .arg("test_branch_1")
        .assert()
        .context("create", "new branch from test_branch_1")
        .success();

    // create without --from and --empty should use the current database
    SERVER
        .admin_cmd()
        .arg("branch")
        .arg("create")
        .arg("test_branch_3")
        .assert()
        .success();

    SERVER
        .admin_cmd()
        .arg("--branch")
        .arg("test_branch_1")
        .arg("branch")
        .arg("current")
        .arg("--plain")
        .assert()
        .context("create", "check the current branch")
        .success()
        .stdout(predicates::str::contains("test_branch_1"));

    // switch without specifying  --instance
    SERVER
        .admin_cmd()
        .arg("branch")
        .arg("switch")
        .arg("test_branch_2")
        .assert()
        .context("switch", "without specifying an instance")
        .failure()
        .stderr(predicates::str::contains(
            "Cannot switch branches without specifying the instance",
        ));

    // switch requires instance name, so let's link the test instance
    let instance_name = SERVER.ensure_instance_linked();

    crate::edgedb_cli_cmd()
        .arg("--instance")
        .arg(instance_name)
        .arg("branch")
        .arg("current")
        .arg("--plain")
        .assert()
        .context("current", "with --instance")
        .success()
        .stdout(predicates::str::contains(default_branch));

    crate::edgedb_cli_cmd()
        .arg("--instance")
        .arg(instance_name)
        .arg("branch")
        .arg("switch")
        .arg("test_branch_2")
        .assert()
        .context("switch", "to test_branch_2 with --instance")
        .success();

    crate::edgedb_cli_cmd()
        .arg("--instance")
        .arg(instance_name)
        .arg("branch")
        .arg("current")
        .arg("--plain")
        .assert()
        .context("current", "with --instance")
        .success()
        .stdout(predicates::str::contains("test_branch_2"));

    // Drop test_branch_2 from main
    crate::edgedb_cli_cmd()
        .arg("--instance")
        .arg(instance_name)
        .arg("-b")
        .arg("main")
        .arg("query")
        .arg("drop branch test_branch_2")
        .assert()
        .context("drop", "drop current branch")
        .success();

    crate::edgedb_cli_cmd()
        .arg("--instance")
        .arg(instance_name)
        .arg("branch")
        .arg("switch")
        .arg("main")
        .assert()
        .context("switch", "when current branch is destroyed")
        .success();

    // setup a project (first unlink, so repeated tests work)
    crate::edgedb_cli_cmd()
        .current_dir("tests/proj/project1")
        .arg("project")
        .arg("unlink")
        .arg("--non-interactive")
        .assert()
        .context("project-unlink", "");
    crate::edgedb_cli_cmd()
        .current_dir("tests/proj/project1")
        .arg("project")
        .arg("init")
        .arg("--link")
        .arg("--server-instance")
        .arg(instance_name)
        .arg("--database")
        .arg("test_branch_3")
        .arg("--non-interactive")
        .arg("--no-migrations")
        .assert()
        .context("project-init", "link project to the test instance")
        .success();

    #[track_caller]
    fn get_current_project_branch() -> String {
        let output = crate::edgedb_cli_cmd()
            .current_dir("tests/proj/project1")
            .arg("branch")
            .arg("current")
            .arg("--plain")
            .assert()
            .context("project-current", "get current project branch")
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(output).unwrap().trim().into()
    }

    #[track_caller]
    fn get_current_instance_branch(instance_name: &str) -> String {
        let output = crate::edgedb_cli_cmd()
            .arg("--instance")
            .arg(instance_name)
            .arg("branch")
            .arg("current")
            .arg("--plain")
            .assert()
            .context("instance-current", "get current instance branch")
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(output).unwrap().trim().into()
    }

    // branch current after project init
    assert_eq!(get_current_project_branch(), "test_branch_3");
    assert_eq!(get_current_instance_branch(instance_name), "main");

    // switch project branch
    crate::edgedb_cli_cmd()
        .current_dir("tests/proj/project1")
        .arg("branch")
        .arg("switch")
        .arg("main")
        .assert()
        .context("switch", "switch project to another branch")
        .success();
    assert_eq!(get_current_project_branch(), "main");
    assert_eq!(get_current_instance_branch(instance_name), "main");

    // switch instance branch
    crate::edgedb_cli_cmd()
        .arg("--instance")
        .arg(instance_name)
        .arg("branch")
        .arg("switch")
        .arg("test_branch_1")
        .assert()
        .context("switch", "switch instance branch")
        .success();
    assert_eq!(get_current_project_branch(), "main");
    assert_eq!(get_current_instance_branch(instance_name), "test_branch_1");

    // switch instance branch, but from within a project
    crate::edgedb_cli_cmd()
        .current_dir("tests/proj/project1")
        .arg("--instance")
        .arg(instance_name)
        .arg("branch")
        .arg("switch")
        .arg("test_branch_3")
        .assert()
        .context("switch", "switch instance branch")
        .success();
    assert_eq!(get_current_project_branch(), "main");
    assert_eq!(get_current_instance_branch(instance_name), "test_branch_3");
}

#[test]
fn hash_password() {
    crate::edgedb_cli_cmd()
        .arg("hash-password")
        .arg("password1234")
        .assert()
        .context("hash-password", "basic usage")
        .success()
        .stdout(predicates::str::starts_with("SCRAM-SHA-256$"));
}

#[test]
fn force_database_error() {
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("error_test2")
        .assert()
        .context("create", "create new database")
        .success();

    SERVER
        .admin_cmd()
        .arg("query")
        .arg("--database=error_test2")
        .arg(
            r#"configure current database
                set force_database_error :=
                  '{"type": "QueryError", "message": "ongoing maintenance"}';
            "#,
        )
        .assert()
        .context("set force_database_error", "should succeed")
        .success();

    SERVER
        .admin_cmd()
        .arg("query")
        .arg("--database=error_test2")
        .arg(
            r#"configure current database
                reset force_database_error;
            "#,
        )
        .assert()
        .context("reset force_database_error", "should succeed")
        .success();
}

#[test]
fn warnings() {
    SERVER
        .admin_cmd()
        .arg("query")
        .arg("select std::_warn_on_call();")
        .assert()
        .stderr(
            predicates::str::contains(r#"warning"#)
                .and(predicates::str::contains("std::_warn_on_call"))
                .and(predicates::str::contains("^^^^^^^^^^^^^^^^^^^^"))
                .and(predicates::str::contains("Test warning please ignore"))
                .into_output(),
        )
        .context("warnings", "print warning from _warn_on_call")
        .success();

    crate::rm_migration_files("tests/migrations/db6", &[1]);
    SERVER
        .admin_cmd()
        .arg("branch")
        .arg("create")
        .arg("--empty")
        .arg("test_warnings")
        .assert()
        .success();

    // test that warning is printed during "migration create"
    SERVER
        .admin_cmd()
        .arg("--branch=test_warnings")
        .arg("migration")
        .arg("create")
        .arg("--schema-dir=tests/migrations/db6")
        .assert()
        .success()
        .stderr(
            predicates::str::contains(r#"warning"#)
                // assert correct path and position, regardless of other files in the schema (d.esdl)
                .and(predicates::str::contains("db6/default.esdl:3:23"))
                .and(predicates::str::contains("std::_warn_on_call"))
                .and(predicates::str::contains("^^^^^^^^^^^^^^^^^^^^"))
                .and(predicates::str::contains("Test warning please ignore")),
        )
        .context("warnings", "print warnings from migrations");
}
