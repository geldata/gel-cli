use crate::{Config, SERVER, util::OutputExt};
use std::error::Error;

#[test]
fn simple_query() {
    let mut cmd = SERVER.admin_interactive();
    let main = SERVER.default_branch();

    cmd.exp_string(&format!("{main}>")).unwrap();
    cmd.send_line("SELECT 'abc'++'def';\n").unwrap();
    cmd.exp_string("abcdef").unwrap();
    cmd.exp_string(&format!("{main}>")).unwrap();
    cmd.send_line(" SELECT 'xy'++'z';\n").unwrap();
    cmd.exp_string("xyz").unwrap();
}

#[test]
fn two_queries() {
    let mut cmd = SERVER.admin_interactive();
    let main = SERVER.default_branch();

    cmd.exp_string(&format!("{main}>")).unwrap();
    cmd.send_line("SELECT 'AB'++'C'; SELECT 'XY'++'Z';\n")
        .unwrap();
    cmd.exp_string("ABC").unwrap();
    cmd.exp_string("XYZ").unwrap();
}

#[test]
fn test_switch_database() {
    let mut cmd = SERVER.admin_interactive();
    let main = SERVER.default_branch();

    cmd.exp_string(&format!("{main}>")).unwrap();
    cmd.send_line("create database _test_switch_asdf;").unwrap();
    cmd.send_line("\\c _test_switch_asdf").unwrap();
    cmd.exp_string("_test_switch_asdf").unwrap();
    cmd.exp_string(">").unwrap();
    cmd.send_line(&format!("\\c {main}")).unwrap();
    cmd.exp_string(&format!("{main}>")).unwrap();
    cmd.send_line("drop branch _test_switch_asdf;").unwrap();
}

#[test]
fn create_report() {
    let mut cmd = SERVER.admin_interactive();
    let main = SERVER.default_branch();

    cmd.exp_string(&format!("{main}>")).unwrap();
    cmd.send_line("CREATE TYPE default::Type1;\n").unwrap();
    cmd.exp_string("OK: ").unwrap();
    cmd.exp_string("CREATE").unwrap();
}

#[test]
fn configured_limit() -> Result<(), Box<dyn Error>> {
    let main = SERVER.default_branch();

    let config = Config::new(
        r###"
[shell]
limit = 2
"###,
    );
    let mut cmd = SERVER.custom_interactive(|cmd| {
        cmd.env("XDG_CONFIG_HOME", config.path());
    });
    cmd.exp_string(&format!("{main}>")).unwrap();
    cmd.send_line("SELECT {'abc', 'def', 'fgh'};\n").unwrap();
    cmd.exp_string("...").unwrap();

    let config = Config::new(
        r###"
[shell]
limit = 3
"###,
    );
    let mut cmd = SERVER.custom_interactive(|cmd| {
        cmd.env("XDG_CONFIG_HOME", config.path());
    });
    cmd.exp_string(&format!("{main}>")).unwrap();
    cmd.send_line("SELECT {'abc', 'def', 'fgh'};\n").unwrap();
    cmd.exp_string("{").unwrap();
    cmd.exp_string("fgh").unwrap();

    Ok(())
}

#[test]
fn warnings() {
    let mut cmd = SERVER.admin_interactive();
    let main = SERVER.default_branch();

    cmd.exp_string(&format!("{main}>")).unwrap();
    cmd.send_line("select std::_warn_on_call();\n").unwrap();
    cmd.exp_string("warning").unwrap();
    cmd.exp_string("std::_warn_on_call()").unwrap();
    cmd.exp_string("^^^^^^^^^^^^^^^^^^^^").unwrap();
    cmd.exp_string("Test warning please ignore").unwrap();
    cmd.exp_string("0").unwrap();
}
