use crate::{BRANDING_CLI_CMD, SERVER};
use predicates::boolean::PredicateBooleanExt;
use predicates::str::{contains, ends_with};
use std::fs;
use std::path::Path;

#[test]
fn bare_status() {
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("empty")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=empty")
        .arg("migration")
        .arg("status")
        .arg("--schema-dir=tests/migrations/db1/bare")
        .assert()
        .code(2)
        .stderr(contains("CREATE PROPERTY field1"))
        .stderr(contains(format!(
            "{BRANDING_CLI_CMD} error: Some migrations are missing"
        )));
}

#[test]
fn initial() {
    crate::rm_migration_files("tests/migrations/db1/initial", &[2, 3]);

    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("initial")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migration")
        .arg("status")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .assert()
        .code(3)
        .stderr(ends_with(format!(
            "{BRANDING_CLI_CMD} error: Database is empty, while 1 migrations \
            have been found in the filesystem.\n  Run `{BRANDING_CLI_CMD} migrate` to apply.\n"
        )));
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("query")
        .arg("SELECT cfg::DatabaseConfig.allow_bare_ddl")
        .assert()
        .success()
        .stdout("\"AlwaysAllow\"\n");
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migration")
        .arg("create")
        .arg("--non-interactive")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .assert()
        .code(1)
        .stderr(ends_with(format!(
            "{BRANDING_CLI_CMD} error: Database must be updated \
            to the last migration on the filesystem for `migration create`. \
            Run:\n  \
              {BRANDING_CLI_CMD} migrate\n",
        )));
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa \
            (00001-m12bulr.edgeql)\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migration")
        .arg("log")
        .arg("--from-db")
        .arg("--newest-first")
        .arg("--limit=1")
        .assert()
        .code(0)
        .stdout("m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa\n");
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("query")
        .arg("SELECT cfg::DatabaseConfig.allow_bare_ddl")
        .assert()
        .success()
        .stdout("\"NeverAllow\"\n");
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migration")
        .arg("status")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .assert()
        .success()
        .stderr(ends_with(
            "Database is up to date. \
            Last migration: \
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa.\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migration")
        .arg("create")
        .arg("--non-interactive")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .assert()
        .code(4)
        .stderr(ends_with("No schema changes detected.\n"));
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migration")
        .arg("create")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .assert()
        .code(4)
        .stderr(ends_with("No schema changes detected.\n"));

    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migration")
        .arg("create")
        .arg("--allow-empty")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .assert()
        .code(0)
        .stderr(ends_with(
            "Created \
            tests/migrations/db1/initial/migrations/00002-m1e5vq3.edgeql, \
            id: m1e5vq3h4oizlsp4a3zge5bqhu7yeoorc27k3yo2aaenfqgfars6uq\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m1e5vq3h4oizlsp4a3zge5bqhu7yeoorc27k3yo2aaenfqgfars6uq \
            (00002-m1e5vq3.edgeql)\n",
        ));

    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migration")
        .arg("create")
        .arg("--allow-empty")
        .arg("--non-interactive")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .assert()
        .code(0);
    SERVER
        .admin_cmd()
        .arg("--branch=initial")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m1wrvvw3lycyovtlx4szqm75554g75h5nnbjq3a5qsdncn3oef6nia \
            (00003-m1wrvvw.edgeql)\n",
        ));

    // Now test partial migrations
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("initial_2")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=initial_2")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .arg("--to-revision=m1e5vq3h4oizlsp4a3zge5bqh")
        .assert()
        .success()
        .stderr(
            contains(
                "Applying \
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa \
            (00001-m12bulr.edgeql)\n",
            )
            .and(contains(
                "Applying \
            m1e5vq3h4oizlsp4a3zge5bqhu7yeoorc27k3yo2aaenfqgfars6uq \
            (00002-m1e5vq3.edgeql)\n",
            )),
        );

    SERVER
        .admin_cmd()
        .arg("--branch=initial_2")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .arg("--to-revision=m12bulrbo")
        .assert()
        .success()
        .stderr(ends_with(
            "Database is up to date. \
            Revision m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa \
            is the ancestor of the latest \
            m1e5vq3h4oizlsp4a3zge5bqhu7yeoorc27k3yo2aaenfqgfars6uq\n",
        ));

    SERVER
        .admin_cmd()
        .arg("--branch=initial_2")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .arg("--to-revision=m1e5vq3h4oizlsp4a")
        .assert()
        .success()
        .stderr(ends_with(
            "Database is up to date. Revision \
            m1e5vq3h4oizlsp4a3zge5bqhu7yeoorc27k3yo2aaenfqgfars6uq\n",
        ));

    SERVER
        .admin_cmd()
        .arg("--branch=initial_2")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/initial")
        .arg("--to-revision=m1wrvvw3lycy")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m1wrvvw3lycyovtlx4szqm75554g75h5nnbjq3a5qsdncn3oef6nia \
            (00003-m1wrvvw.edgeql)\n",
        ));
}

#[test]
fn project() {
    crate::rm_migration_files("tests/migrations/db1/project/priv/dbschema", &[2, 3]);

    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("project")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("migration")
        .arg("status")
        .current_dir("tests/migrations/db1/project")
        .assert()
        .code(3)
        .stderr(ends_with(format!(
            "{BRANDING_CLI_CMD} error: Database is empty, while 1 migrations \
            have been found in the filesystem.\n  Run `{BRANDING_CLI_CMD} migrate` to apply.\n",
        )));
    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("query")
        .arg("SELECT cfg::DatabaseConfig.allow_bare_ddl")
        .assert()
        .success()
        .stdout("\"AlwaysAllow\"\n");
    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("migration")
        .arg("create")
        .arg("--non-interactive")
        .current_dir("tests/migrations/db1/project")
        .assert()
        .code(1)
        .stderr(ends_with(format!(
            "{BRANDING_CLI_CMD} error: Database must be updated \
            to the last migration on the filesystem for `migration create`. \
            Run:\n  \
              {BRANDING_CLI_CMD} migrate\n",
        )));
    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("migrate")
        .current_dir("tests/migrations/db1/project")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa \
            (00001-m12bulr.edgeql)\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("query")
        .arg("SELECT cfg::DatabaseConfig.allow_bare_ddl")
        .assert()
        .success()
        .stdout("\"NeverAllow\"\n");
    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("migration")
        .arg("status")
        .current_dir("tests/migrations/db1/project")
        .assert()
        .success()
        .stderr(ends_with(
            "Database is up to date. \
            Last migration: \
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa.\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("migration")
        .arg("create")
        .arg("--non-interactive")
        .current_dir("tests/migrations/db1/project")
        .assert()
        .code(4)
        .stderr(ends_with("No schema changes detected.\n"));
    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("migration")
        .arg("create")
        .current_dir("tests/migrations/db1/project")
        .assert()
        .code(4)
        .stderr(ends_with("No schema changes detected.\n"));

    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("migration")
        .arg("create")
        .arg("--allow-empty")
        .current_dir("tests/migrations/db1/project")
        .assert()
        .code(0)
        .stderr(ends_with(
            "Created \
            priv/dbschema/migrations/00002-m1e5vq3.edgeql, \
            id: m1e5vq3h4oizlsp4a3zge5bqhu7yeoorc27k3yo2aaenfqgfars6uq\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("migrate")
        .current_dir("tests/migrations/db1/project")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m1e5vq3h4oizlsp4a3zge5bqhu7yeoorc27k3yo2aaenfqgfars6uq \
            (00002-m1e5vq3.edgeql)\n",
        ));

    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("migration")
        .arg("create")
        .arg("--allow-empty")
        .arg("--non-interactive")
        .current_dir("tests/migrations/db1/project")
        .assert()
        .code(0);
    SERVER
        .admin_cmd()
        .arg("--branch=project")
        .arg("migrate")
        .current_dir("tests/migrations/db1/project")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m1wrvvw3lycyovtlx4szqm75554g75h5nnbjq3a5qsdncn3oef6nia \
            (00003-m1wrvvw.edgeql)\n",
        ));

    // Now test partial migrations
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("project_2")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=project_2")
        .arg("migrate")
        .current_dir("tests/migrations/db1/project")
        .arg("--to-revision=m1e5vq3h4oizlsp4a3zge5bqh")
        .assert()
        .success()
        .stderr(
            contains(
                "Applying \
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa \
            (00001-m12bulr.edgeql)\n",
            )
            .and(contains(
                "Applying \
            m1e5vq3h4oizlsp4a3zge5bqhu7yeoorc27k3yo2aaenfqgfars6uq \
            (00002-m1e5vq3.edgeql)\n",
            )),
        );

    SERVER
        .admin_cmd()
        .arg("--branch=project_2")
        .arg("migrate")
        .current_dir("tests/migrations/db1/project")
        .arg("--to-revision=m12bulrbo")
        .assert()
        .success()
        .stderr(ends_with(
            "Database is up to date. \
            Revision m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa \
            is the ancestor of the latest \
            m1e5vq3h4oizlsp4a3zge5bqhu7yeoorc27k3yo2aaenfqgfars6uq\n",
        ));

    SERVER
        .admin_cmd()
        .arg("--branch=project_2")
        .arg("migrate")
        .current_dir("tests/migrations/db1/project")
        .arg("--to-revision=m1e5vq3h4oizlsp4a")
        .assert()
        .success()
        .stderr(ends_with(
            "Database is up to date. Revision \
            m1e5vq3h4oizlsp4a3zge5bqhu7yeoorc27k3yo2aaenfqgfars6uq\n",
        ));

    SERVER
        .admin_cmd()
        .arg("--branch=project_2")
        .arg("migrate")
        .current_dir("tests/migrations/db1/project")
        .arg("--to-revision=m1wrvvw3lycy")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m1wrvvw3lycyovtlx4szqm75554g75h5nnbjq3a5qsdncn3oef6nia \
            (00003-m1wrvvw.edgeql)\n",
        ));
}

#[test]
fn modified1() {
    crate::rm_migration_files("tests/migrations/db1/modified1", &[2]);

    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("modified1")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migration")
        .arg("status")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .code(3)
        .stderr(ends_with(format!(
            "{BRANDING_CLI_CMD} error: Database is empty, while 1 migrations \
            have been found in the filesystem.\n  Run `{BRANDING_CLI_CMD} migrate` to apply.\n",
        )));
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migration")
        .arg("create")
        .arg("--non-interactive")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .code(1)
        .stderr(ends_with(format!(
            "{BRANDING_CLI_CMD} error: Database must be updated \
            to the last migration on the filesystem for `migration create`. \
            Run:\n  \
              {BRANDING_CLI_CMD} migrate\n",
        )));
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa \
            (00001-m12bulr.edgeql)\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migration")
        .arg("status")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .code(2)
        .stderr(contains("CREATE PROPERTY field2"))
        .stderr(contains(format!(
            "{BRANDING_CLI_CMD} error: Some migrations are missing"
        )));
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migration")
        .arg("create")
        .arg("--non-interactive")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .code(0);
    SERVER
        .admin_cmd()
        .arg("migration")
        .arg("log")
        .arg("--from-fs")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .arg("--newest-first")
        .arg("--limit=1")
        .assert()
        .code(0)
        .stdout("m13wjyiog2dbum2ou32yp77eysbewews7vlv6rqqfswpyi2yd4s55a\n");
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migration")
        .arg("status")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .code(3)
        .stderr(ends_with(
            "Database is at migration \
            \"m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa\" \
            while sources contain 1 migrations ahead, \
            starting from \
            \"m13wjyiog2dbum2ou32yp77eysbewews7vlv6rqqfswpyi2yd4s55a\"\
            (tests/migrations/db1/modified1/migrations/00002-m13wjyi.edgeql)\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m13wjyiog2dbum2ou32yp77eysbewews7vlv6rqqfswpyi2yd4s55a \
            (00002-m13wjyi.edgeql)\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .code(0)
        .stderr(ends_with(
            "Everything is up to date. Revision \
            m13wjyiog2dbum2ou32yp77eysbewews7vlv6rqqfswpyi2yd4s55a\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migration")
        .arg("status")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .success()
        .stderr(ends_with(
            "Database is up to date. \
            Last migration: \
            m13wjyiog2dbum2ou32yp77eysbewews7vlv6rqqfswpyi2yd4s55a.\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migration")
        .arg("create")
        .arg("--non-interactive")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .code(4)
        .stderr(ends_with("No schema changes detected.\n"));
    SERVER
        .admin_cmd()
        .arg("migration")
        .arg("log")
        .arg("--from-fs")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .arg("--newest-first")
        .assert()
        .code(0)
        .stdout(
            "\
            m13wjyiog2dbum2ou32yp77eysbewews7vlv6rqqfswpyi2yd4s55a\n\
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa\n\
        ",
        );
    SERVER
        .admin_cmd()
        .arg("migration")
        .arg("log")
        .arg("--from-fs")
        .arg("--schema-dir=tests/migrations/db1/modified1")
        .assert()
        .code(0)
        .stdout(
            "\
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa\n\
            m13wjyiog2dbum2ou32yp77eysbewews7vlv6rqqfswpyi2yd4s55a\n\
        ",
        );

    fs::remove_dir_all("tests/migrations/db1/squash").ok();
    fs::create_dir_all("tests/migrations/db1/squash").unwrap();
    fs::create_dir_all("tests/migrations/db1/squash/migrations").unwrap();
    fs::copy(
        "tests/migrations/db1/modified1/default.esdl",
        "tests/migrations/db1/squash/default.esdl",
    )
    .unwrap();
    fs::copy(
        "tests/migrations/db1/modified1/migrations/00001-m12bulr.edgeql",
        "tests/migrations/db1/squash/migrations/00001-m12bulr.edgeql",
    )
    .unwrap();
    fs::copy(
        "tests/migrations/db1/modified1/migrations/00002-m13wjyi.edgeql",
        "tests/migrations/db1/squash/migrations/00002-m13wjyi.edgeql",
    )
    .unwrap();

    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migration")
        .arg("create")
        .arg("--squash")
        .arg("--non-interactive")
        .arg("--schema-dir=tests/migrations/db1/squash")
        .assert()
        .success()
        .stderr(contains("Squash is complete"));
    SERVER
        .admin_cmd()
        .arg("--branch=modified1")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/squash")
        .assert()
        .success()
        .stderr(contains(
            "m1fw3q62du3fmdbeuikq3tc4fsfhs3phafnjhoh3jzedk3sfgx3lha\
            .edgeql)\n",
        ));
    SERVER
        .admin_cmd()
        .arg("migration")
        .arg("log")
        .arg("--from-fs")
        .arg("--schema-dir=tests/migrations/db1/squash")
        .assert()
        .code(0)
        .stdout(
            "\
            m1fw3q62du3fmdbeuikq3tc4fsfhs3phafnjhoh3jzedk3sfgx3lha\n\
        ",
        );
    SERVER
        .admin_cmd()
        .arg("migration")
        .arg("log")
        .arg("--branch=modified1")
        .arg("--from-db")
        .arg("--schema-dir=tests/migrations/db1/squash")
        .assert()
        .code(0)
        .stdout(
            "\
            m1fw3q62du3fmdbeuikq3tc4fsfhs3phafnjhoh3jzedk3sfgx3lha\n\
        ",
        );
}

#[test]
fn error() {
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("empty_err")
        .assert()
        .success();
    let err = if SERVER.0.version_major.is_at_least(6) {
        format!(
            r###"error: Unexpected keyword 'CREATE'
  ┌─ tests/migrations/db1/error/bad.esdl:3:9
  │
3 │         create property text -> str;
  │         ^^^^^^ Use a different identifier or quote the name with backticks: `create`
  │
  = This name is a reserved keyword and cannot be used as an identifier

{BRANDING_CLI_CMD} error: cannot proceed until schema files are fixed
"###
        )
    } else if SERVER.0.version_major.is_at_least(4) {
        format!(
            r###"error: Unexpected keyword 'CREATE'
  ┌─ tests/migrations/db1/error/bad.esdl:3:9
  │
3 │         create property text -> str;
  │         ^^^^^^ Use a different identifier or quote the name with backticks: `create`
  │
  = This name is a reserved keyword and cannot be used as an identifier

{BRANDING_CLI_CMD} error: cannot proceed until .esdl files are fixed
"###
        )
    } else {
        format!(
            r###"error: Unexpected keyword 'CREATE'
  ┌─ tests/migrations/db1/error/bad.esdl:3:9
  │
3 │         create property text -> str;
  │         ^^^^^^ error

{BRANDING_CLI_CMD} error: cannot proceed until .esdl files are fixed
"###
        )
    };
    SERVER
        .admin_cmd()
        .arg("--branch=empty_err")
        .arg("migration")
        .arg("status")
        .arg("--schema-dir=tests/migrations/db1/error")
        .env("NO_COLOR", "1")
        .assert()
        .code(1)
        .stderr(ends_with(err));
}

#[test]
fn modified2_interactive() {
    crate::rm_migration_files("tests/migrations/db1/modified2", &[2]);
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("modified2")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=modified2")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/modified2")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa \
            (00001-m12bulr.edgeql)\n",
        ));

    let mut cmd = SERVER.custom_interactive(|cmd| {
        cmd.arg("--branch=modified2");
        cmd.arg("migration").arg("create");
        cmd.arg("--schema-dir=tests/migrations/db1/modified2");
    });
    cmd.exp_string("[y,n,l,c,b,s,q,?]").unwrap();
    cmd.send_line("y").unwrap();
    cmd.exp_string(
        "Created \
        tests/migrations/db1/modified2/migrations/00002-m13wjyi.edgeql, \
        id: m13wjyiog2dbum2ou32yp77eysbewews7vlv6rqqfswpyi2yd4s55a",
    )
    .unwrap();

    SERVER
        .admin_cmd()
        .arg("--branch=modified2")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/modified2")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m13wjyiog2dbum2ou32yp77eysbewews7vlv6rqqfswpyi2yd4s55a \
            (00002-m13wjyi.edgeql)\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=modified2")
        .arg("migration")
        .arg("status")
        .arg("--schema-dir=tests/migrations/db1/modified2")
        .assert()
        .success()
        .stderr(ends_with(
            "Database is up to date. \
            Last migration: \
            m13wjyiog2dbum2ou32yp77eysbewews7vlv6rqqfswpyi2yd4s55a.\n",
        ));
    SERVER
        .admin_cmd()
        .arg("--branch=modified2")
        .arg("migration")
        .arg("create")
        .arg("--schema-dir=tests/migrations/db1/modified2")
        .assert()
        .code(4)
        .stderr(ends_with("No schema changes detected.\n"));
}

#[test]
fn modified3_interactive() {
    crate::rm_migration_files("tests/migrations/db1/modified3", &[2]);
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("modified3")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=modified3")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/modified3")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m12bulrbounwj3oj5xsspa7gj676azrog6ndi45iyuwrwzvawkxraa \
            (00001-m12bulr.edgeql)\n",
        ));

    let mut cmd = SERVER.custom_interactive(|cmd| {
        cmd.arg("--branch=modified3");
        cmd.arg("migration").arg("create").arg("--expert");
        cmd.arg("--schema-dir=tests/migrations/db1/modified3");
    });
    cmd.exp_string("[y,n,l,c,b,s,q,?]").unwrap();
    cmd.send_line("yes").unwrap();
    cmd.exp_string("[y,n,l,c,b,s,q,?]").unwrap();
    cmd.send_line("yes").unwrap();
    cmd.exp_string("[y,n,l,c,b,s,q,?]").unwrap();
    cmd.send_line("back").unwrap();
    cmd.exp_string("[y,n,l,c,b,s,q,?]").unwrap();
    cmd.send_line("yes").unwrap();
    cmd.exp_string("[y,n,l,c,b,s,q,?]").unwrap();
    cmd.send_line("yes").unwrap();
    cmd.exp_string("Created").unwrap();

    SERVER
        .admin_cmd()
        .arg("--branch=modified3")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db1/modified3")
        .assert()
        .success(); // revision can be different because of order
    SERVER
        .admin_cmd()
        .arg("--branch=modified3")
        .arg("migration")
        .arg("status")
        .arg("--schema-dir=tests/migrations/db1/modified3")
        .assert()
        .success(); // revision can be different because of order
    SERVER
        .admin_cmd()
        .arg("--branch=modified3")
        .arg("migration")
        .arg("create")
        .arg("--non-interactive")
        .arg("--schema-dir=tests/migrations/db1/modified3")
        .assert()
        .code(4)
        .stderr(ends_with("No schema changes detected.\n"));
}

#[test]
fn prompt_id() {
    crate::rm_migration_files("tests/migrations/db2/initial", &[1]);
    crate::rm_migration_files("tests/migrations/db2/modified1", &[2]);

    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("db2")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=db2")
        .arg("migration")
        .arg("create")
        .arg("--schema-dir=tests/migrations/db2/initial")
        .assert()
        .success(); // should not ask any questions on first rev
    SERVER
        .admin_cmd()
        .arg("--branch=db2")
        .arg("migrate")
        .arg("--schema-dir=tests/migrations/db2/initial")
        .assert()
        .success()
        .stderr(contains(
            "Applying \
            m1fvz72asuad3xkor4unxshp524wp6stgdnbd34vxvjfjkrzemonkq \
            (00001-m1fvz72.edgeql)\n",
        ));

    let mut cmd = SERVER.custom_interactive(|cmd| {
        cmd.arg("--branch=db2");
        cmd.arg("migration").arg("create").arg("--expert");
        cmd.arg("--schema-dir=tests/migrations/db2/modified1");
    });
    cmd.exp_string("[y,n,l,c,b,s,q,?]").unwrap();
    cmd.send_line("yes").unwrap();
    cmd.exp_string("[y,n,l,c,b,s,q,?]").unwrap();
    cmd.send_line("yes").unwrap();
    // on pre-prompt_id version this would require an extra prompt
    cmd.exp_string("extra DDL statements").unwrap();
    cmd.exp_string("Created").unwrap();
}

#[test]
fn eof_err() {
    let err = if SERVER.0.version_major.is_at_least(6) {
        format!(
            r###"error: Missing '{{'
   ┌─ tests/migrations/db_eof_err/default.esdl:9:19
   │  
 9 │   alias default::Foo
   │ ╭──────────────────^
10 │ │ 
   │ ╰^ error

{BRANDING_CLI_CMD} error: cannot proceed until schema files are fixed
"###
        )
    } else {
        format!(
            r###"error: Missing '{{'
   ┌─ tests/migrations/db_eof_err/default.esdl:9:19
   │  
 9 │   alias default::Foo
   │ ╭──────────────────^
10 │ │ 
   │ ╰^ error

{BRANDING_CLI_CMD} error: cannot proceed until .esdl files are fixed
"###
        )
    };
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("db_eof_err")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=db_eof_err")
        .arg("migration")
        .arg("create")
        .arg("--schema-dir=tests/migrations/db_eof_err")
        .env("NO_COLOR", "1")
        .assert()
        .code(1)
        .stderr(ends_with(err));
}

#[test]
fn dev_mode() {
    crate::rm_migration_files("tests/migrations/db4/modified1", &[1]);
    crate::rm_migration_files("tests/migrations/db4/created1", &[2]);

    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("db4")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=db4")
        .arg("migrate")
        .arg("--dev-mode")
        .arg("--schema-dir=tests/migrations/db4/initial")
        .env("NO_COLOR", "1")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=db4")
        .arg("migrate")
        .arg("--dev-mode")
        .arg("--schema-dir=tests/migrations/db4/modified1")
        .env("NO_COLOR", "1")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=db4")
        .arg("migration")
        .arg("create")
        .arg("--non-interactive")
        .arg("--schema-dir=tests/migrations/db4/modified1")
        .env("NO_COLOR", "1")
        .assert()
        .success();
    assert!(Path::new("tests/migrations/db4/modified1/migrations/00001-m1qfgvb.edgeql").exists());
    SERVER
        .admin_cmd()
        .arg("--branch=db4")
        .arg("migrate")
        .arg("--dev-mode")
        .arg("--schema-dir=tests/migrations/db4/created1")
        .env("NO_COLOR", "1")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=db4")
        .arg("migration")
        .arg("create")
        .arg("--non-interactive")
        .arg("--schema-dir=tests/migrations/db4/created1")
        .env("NO_COLOR", "1")
        .assert()
        .success();

    assert!(Path::new("tests/migrations/db4/created1/migrations/00002-m1dexnj.edgeql").exists());
}

#[test]
fn unsafe_migrations() {
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("db_unsafe")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=db_unsafe")
        .arg("migrate")
        .arg("--dev-mode")
        .arg("--schema-dir=tests/migrations/db_unsafe/initial")
        .env("NO_COLOR", "1")
        .assert()
        .success();
    SERVER
        .admin_cmd()
        .arg("--branch=db_unsafe")
        .arg("migrate")
        .arg("--dev-mode")
        .arg("--schema-dir=tests/migrations/db_unsafe/modified1")
        .env("NO_COLOR", "1")
        .assert()
        .success();
}
