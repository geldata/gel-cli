use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::{env, fs};

enum Platform {
    Linux,
    Windows,
    MacOS,
}

macro_rules! write {
    ($output:tt, $($arg:tt)*) => {{
        $output.write_all(format!($($arg)*).as_bytes()).unwrap();
    }}
}

fn main() {
    let opt_key_mapping = HashMap::from([
        ("credentialsFile", "--credentials-file"),
        ("credentials", "--credentials-file"),
        ("dsn", "--dsn"),
        ("instance", "--instance"),
        ("host", "--host"),
        ("port", "--port"),
        ("database", "--database"),
        ("branch", "--branch"),
        ("user", "--user"),
        ("tlsSecurity", "--tls-security"),
        ("tlsCA", "--tls-ca-file"),
        ("tlsCAFile", "--tls-ca-file"),
        ("tlsServerName", "--tls-server-name"),
        ("waitUntilAvailable", "--wait-until-available"),
        ("secretKey", "--secret-key"),
    ]);
    let error_mapping = HashMap::from([
        ("invalid_dsn", "(invalid DSN|Invalid DSN)"),
        ("env_not_found", "is not set"),
        (
            "invalid_tls_security",
            "((EDGEDB_CLIENT_TLS_SECURITY|tls_security).*(don't comply|Invalid value)|\
            Unsupported TLS security|Insecure TLS configuration is not allowed in strict mode)",
        ),
        (
            "file_not_found",
            "(No such file or directory)|(cannot find the path)|\
            (a value is required for)",
        ),
        ("invalid_host", "invalid host"),
        (
            "invalid_port",
            "(invalid value.*for.*port|invalid port|EDGEDB_PORT is invalid)",
        ),
        (
            "invalid_dsn_or_instance_name",
            "(invalid DSN|must be a valid identifier)",
        ),
        ("invalid_instance_name", "invalid.*instance name"),
        ("invalid_user", "invalid user"),
        ("invalid_database", "invalid database"),
        (
            "invalid_credentials_file",
            "(cannot read credentials file)|(a value is required for)",
        ),
        (
            "no_options_or_toml",
            "no .*toml.* found and no connection options are specified",
        ),
        (
            "multiple_compound_opts",
            "(cannot be used with)|(provided more than once)|(cannot be used multiple times)",
        ),
        (
            "multiple_compound_env",
            "multiple compound env vars found|\
             [A-Z]+\\s*conflicts\\s*with\\s*[A-Z]+",
        ),
        (
            "exclusive_options",
            "(provided more than once)|(cannot be used multiple times)|(are mutually exclusive)",
        ),
        (
            "credentials_file_not_found",
            "credentials file.*No such file",
        ),
        ("project_not_initialised", "not initialized"),
        ("secret_key_not_found", "NoCloudConfigFound"),
        ("invalid_secret_key", "Illegal JWT token"),
    ]);

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let out_file = Path::new(&out_dir).join("shared_client_testcases.rs");
    let out_file = fs::File::create(out_file).unwrap();
    let mut output = BufWriter::new(out_file);

    let root = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let testcases = Path::new(&root).join("..").join("shared-client-testcases");

    let connection_testcases = testcases.join("connection_testcases.json");
    println!(
        "cargo:rerun-if-changed={}",
        connection_testcases.to_str().unwrap()
    );
    let connection_testcases = fs::read_to_string(connection_testcases)
        .expect("Shared test git submodule is missing: ensure git checkout includes submodules with --recurse-submodules");
    let connection_testcases: Value = serde_json::from_str(&connection_testcases).unwrap();
    let empty_map = Map::new();
    write!(
        output,
        "
use std::sync::Mutex;

static MUTEX: Mutex<()> = Mutex::new(());
"
    );

    'testcase: for case in connection_testcases.as_array().unwrap() {
        let mut testcase = Vec::new();
        let case = case.as_object().unwrap();
        let name = case.get("name").unwrap().as_str().unwrap();
        let opts = case
            .get("opts")
            .and_then(|v| v.as_object())
            .filter(|m| !m.is_empty());
        let env = case
            .get("env")
            .and_then(|v| v.as_object())
            .filter(|m| !m.is_empty());
        let fs = case
            .get("fs")
            .and_then(|v| v.as_object())
            .unwrap_or(&empty_map);
        let platform = match case.get("platform").and_then(|p| p.as_str()) {
            Some("macos") => {
                write!(testcase, "#[cfg(target_os=\"macos\")]");
                Some(Platform::MacOS)
            }
            Some("windows") => {
                write!(testcase, "#[cfg(target_os=\"windows\")]");
                Some(Platform::Windows)
            }
            _ if !fs.is_empty() => {
                write!(testcase, "#[cfg(target_os=\"linux\")]");
                Some(Platform::Linux)
            }
            _ => None,
        };
        let result = case.get("result").map(|r| r.to_string());

        write!(
            testcase,
            r#"
#[test]
"#
        );

        let mut should_panic = false;
        if let Some(opts) = &opts {
            if let Some(dsn) = opts.get("dsn") {
                if let Some(dsn) = dsn.as_str() {
                    // servo/rust-url#424
                    if dsn.contains("%25eth0")
                        || dsn.starts_with("edgedbadmin://")
                        || dsn.contains("host=/")
                    {
                        should_panic = true;
                    }
                }
            }
            if let Some(host) = opts.get("host") {
                if let Some(host) = host.as_str() {
                    if host.starts_with('/') {
                        should_panic = true;
                    }
                }
            }
        }
        if should_panic {
            write!(
                testcase,
                r#"
#[should_panic]
"#
            );
        }

        write!(
            testcase,
            r#"
fn {name}() {{
    let _mutex = MUTEX.lock();
"#
        );
        if let Some(result) = &result {
            write!(
                testcase,
                r#"
    let result: Value = serde_json::from_str({result:?}).unwrap();"#
            );
        }

        let mut buf = Vec::new();
        if let Some(opts) = opts {
            for (key, value) in opts {
                let arg = if key == "instance" {
                    let argv = value.as_str().unwrap();
                    write!(
                        buf,
                        r#"
        .arg("--instance={argv}")"#,
                    );
                    continue;
                } else if let Some(arg) = opt_key_mapping.get(key.as_str()) {
                    arg
                } else if key == "serverSettings" {
                    continue 'testcase;
                } else if key == "password" {
                    let argv = format!("{}\n", value.as_str().unwrap());
                    write!(
                        buf,
                        r#"
        .arg("--password-from-stdin")
        .write_stdin({argv:?})"#,
                    );
                    continue;
                } else {
                    panic!("unknown opts key: {}", key);
                };
                let argv = if key == "credentials" {
                    let value = value.as_str().unwrap();
                    write!(
                        testcase,
                        r#"
    let mut credentials_file = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
    credentials_file.write_all({value:?}.as_bytes()).unwrap();
    let credentials_file = credentials_file.into_temp_path();
    "#,
                    );
                    "&credentials_file".to_owned()
                } else if key == "tlsCA" {
                    let value = value.as_str().unwrap();
                    write!(
                        testcase,
                        r#"
    let mut tls_ca_file = tempfile::Builder::new().suffix(".pem").tempfile().unwrap();
    tls_ca_file.write_all({value:?}.as_bytes()).unwrap();
    let tls_ca_file = tls_ca_file.into_temp_path();
    "#,
                    );
                    "&tls_ca_file".to_owned()
                } else if let Some(v) = value.as_str() {
                    format!("\"{v}\"")
                } else if let Some(v) = value.as_i64() {
                    format!("\"{v}\"")
                } else if let Some(v) = value.as_f64() {
                    format!("\"{v}\"")
                } else {
                    panic!("invalid value of opts {}: {:?}", key, value);
                };
                write!(
                    buf,
                    r#"
        .arg({arg:?})
        .arg({argv})"#,
                );
            }
        }
        if let Some(env) = env {
            for (key, value) in env {
                let value = if let Some(v) = value.as_str() {
                    v
                } else {
                    panic!("invalid env {} value: {:?}", key, value);
                };
                write!(
                    buf,
                    r#"
        .env({key:?}, {value:?})"#,
                );
            }
        }
        if let Some(cwd) = fs.get("cwd") {
            let mut cwd = cwd.as_str().unwrap().to_string();
            if matches!(platform, Some(Platform::Windows)) {
                cwd = cwd.replace("Users\\edgedb", "Users\\runneradmin");
            }
            write!(
                testcase,
                r#"
    ensure_dir(&PathBuf::from({cwd:?}));
    "#,
            );
            write!(
                buf,
                r#"
        .current_dir({cwd:?})"#,
            );
        }
        if !matches!(platform, Some(Platform::Windows)) {
            if let Some(home) = fs.get("homedir") {
                let home = home.as_str().unwrap();
                write!(
                    buf,
                    r#"
        .env("HOME", {home:?})"#,
                );
            }
        }
        if let Some(files) = fs.get("files") {
            let files = files.as_object().unwrap();
            for (i, (path, value)) in files.iter().enumerate() {
                let mut path = path.clone();
                if matches!(platform, Some(Platform::Windows)) {
                    path = path.replace("Users\\edgedb", "Users\\runneradmin");
                }
                if let Some(content) = value.as_str() {
                    write!(
                        testcase,
                        r#"
    let _file_{i} = mock_file({path:?}, {content:?});
    "#,
                    );
                } else if let Some(d) = value.as_object() {
                    let mut d = d.clone();
                    let mut project_path = d
                        .remove("project-path")
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string();
                    if matches!(platform, Some(Platform::Windows)) {
                        project_path = project_path.replace("Users\\edgedb", "Users\\runneradmin");
                    }
                    let values = d
                        .iter()
                        .map(|(k, v)| format!("{k:?} => {:?}", v.as_str().unwrap()))
                        .collect::<Vec<_>>()
                        .join(",\n        ");
                    write!(
                        testcase,
                        r#"
    let _file_{i} = mock_project({path:?}, {project_path:?}, &indexmap::indexmap! {{
        {values}
    }});
    "#,
                    );
                }
            }
        }

        write!(
            testcase,
            r#"
    Command::cargo_bin("gel").unwrap_or_else(|_| Command::new("gel"))
        .arg("--no-cli-update-check")
        .arg("--test-output-conn-params")"#,
        );
        testcase.write_all(&buf).unwrap();
        if result.is_some() {
            write!(
                testcase,
                r#"
        .assert()
        .success()
        .stdout(expect(result));
}}"#,
            );
        } else {
            let error = case
                .get("error")
                .unwrap()
                .as_object()
                .unwrap()
                .get("type")
                .unwrap()
                .as_str()
                .map(|e| {
                    error_mapping
                        .get(e)
                        .map(|e| e.to_string())
                        .unwrap_or(e.to_string())
                })
                .unwrap();
            write!(
                testcase,
                r#"
        .assert()
        .failure()
        .stderr(predicates::str::is_match({error:?}).unwrap());
}}"#,
            );
        }
        output.write_all(&testcase).unwrap();
    }

    let project_path_hashing_testcases = testcases.join("project_path_hashing_testcases.json");
    println!(
        "cargo:rerun-if-changed={}",
        project_path_hashing_testcases.to_str().unwrap()
    );
    output.flush().unwrap();
}
