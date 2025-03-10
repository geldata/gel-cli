use test_utils::server::ServerInstance;

use crate::{ServerGuard, SERVER};

#[test]
fn dump_restore_cycle() {
    std::fs::create_dir_all("./tmp").expect("can create directory");
    println!("before");
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("dump_01")
        .assert()
        .success();
    println!("dbcreated");
    SERVER
        .database_cmd("dump_01")
        .arg("query")
        .arg("CREATE TYPE Hello { CREATE REQUIRED PROPERTY name -> str; }")
        .arg("INSERT Hello { name := 'world' }")
        .assert()
        .success();
    println!("Created");
    SERVER
        .database_cmd("dump_01")
        .arg("dump")
        .arg("./tmp/dump_01.dump")
        .assert()
        .success();
    println!("dumped");
    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("restore_01")
        .assert()
        .success();
    println!("created2");
    SERVER
        .database_cmd("restore_01")
        .arg("restore")
        .arg("./tmp/dump_01.dump")
        .assert()
        .success();
    println!("restored");
    SERVER
        .database_cmd("restore_01")
        .arg("query")
        .arg("SELECT Hello.name")
        .assert()
        .success()
        .stdout("\"world\"\n");
    println!("query");
}

#[test]
fn dump_all_without_a_format() {
    SERVER
        .admin_cmd()
        .arg("dump")
        .arg("--all")
        .arg("dump01-dir")
        .assert()
        .code(1);
}

#[test]
fn dump_restore_all() {
    let tempdir = tempfile::tempdir().unwrap();

    SERVER
        .admin_cmd()
        .arg("database")
        .arg("create")
        .arg("dump_02")
        .assert()
        .success();
    SERVER
        .database_cmd("dump_02")
        .arg("query")
        .arg("CREATE TYPE Hello { CREATE REQUIRED PROPERTY name -> str; }")
        .arg("INSERT Hello { name := 'world' }")
        .assert()
        .success();

    // dump all databases

    // This might fail if a database gets deleted during the dump
    // so we retry 5 times.
    // We could instead spawn a new edgedb server to increase isolation,
    // but that would slow tests down and they are slow enough already.
    let mut retry = 0;
    let res = loop {
        let r = SERVER
            .admin_cmd()
            .arg("dump")
            .arg("--all")
            .arg("--format=dir")
            .arg(&tempdir.path())
            .ok();
        if r.is_err() && retry < 5 {
            retry += 1;
            continue;
        }
        break r;
    };
    res.unwrap();

    let new_instance = ServerGuard(ServerInstance::start());
    new_instance
        .admin_cmd()
        .arg("restore")
        .arg("--all")
        .arg(&tempdir.path())
        .assert()
        .success();

    new_instance
        .database_cmd("dump_02")
        .arg("query")
        .arg("SELECT Hello.name")
        .assert()
        .success()
        .stdout("\"world\"\n");
    new_instance.0.stop();
}
