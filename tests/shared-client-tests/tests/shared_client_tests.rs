use assert_cmd::Command;
use gel_protocol::model::Duration;
use predicates::reflection::PredicateReflection;
use predicates::Predicate;
use serde_json::Value;
use sha1::Digest;

use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

static MATCHED_PATTERNS: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

struct ResultPredicate {
    result: Value,
}

/// Error patterns for the CLI.
fn error_map() -> HashMap<&'static str, Vec<&'static str>> {
    HashMap::from([
        ("invalid_dsn", vec!["Invalid DSN"]),
        ("env_not_found", vec!["not set"]),
        (
            "invalid_tls_security",
            vec!["Invalid TLS security", "Unsupported TLS security"],
        ),
        (
            "file_not_found",
            vec!["File not found", "but none was supplied"],
        ),
        ("invalid_host", vec!["Invalid host"]),
        (
            "invalid_port",
            vec![
                "Invalid port",
                "invalid digit found in string",
                "cannot parse integer from empty string",
                "is not in",
            ],
        ),
        (
            "invalid_dsn_or_instance_name",
            vec!["Invalid instance name", "Invalid DSN"],
        ),
        ("invalid_instance_name", vec!["invalid.*instance name"]),
        ("invalid_user", vec!["Invalid user"]),
        ("invalid_database", vec!["Invalid database"]),
        (
            "invalid_credentials_file",
            vec!["Invalid credentials file", "but none was supplied"],
        ),
        (
            "no_options_or_toml",
            vec!["no .*toml.* found and no connection options are specified"],
        ),
        (
            "multiple_compound_opts",
            vec!["cannot be used with", "cannot be used multiple times"],
        ),
        (
            "multiple_compound_env",
            vec!["Multiple compound options were specified while parsing environment variables"],
        ),
        (
            "exclusive_options",
            vec!["cannot be used multiple times", "are mutually exclusive"],
        ),
        ("credentials_file_not_found", vec!["file not found"]),
        ("project_not_initialised", vec!["not initialized"]),
        (
            "secret_key_not_found",
            vec!["No Gel Cloud configuration found"],
        ),
        ("invalid_secret_key", vec!["Invalid secret key"]),
        (
            "unix_socket_unsupported",
            vec!["Unix socket unsupported", "must be a hostname"],
        ),
    ])
}

impl Predicate<str> for ResultPredicate {
    fn eval(&self, variable: &str) -> bool {
        let actual: Value = match serde_json::from_str(variable) {
            Ok(value) => value,
            Err(e) => {
                panic!("CLI returned invalid JSON ({:#}): {:?}", e, variable);
            }
        };
        for (k, v) in actual.as_object().unwrap() {
            match self.result.get(k) {
                Some(expected) if k == "waitUntilAvailable" => {
                    let expected = expected.as_str().unwrap().parse::<Duration>().unwrap();
                    if v.as_str().is_none() {
                        panic!("illegal waitUntilAvailable: {}", v);
                    }
                    let v = Duration::from_str(v.as_str().unwrap()).unwrap();
                    if expected != v {
                        println!("{}: {} != {}", k, v, expected);
                        return false;
                    }
                }
                Some(expected) => {
                    if !expected.eq(v) {
                        println!("{}: {} != {}", k, v, expected);
                        return false;
                    }
                }
                None => {
                    println!("{}={} was not expected", k, v);
                    return false;
                }
            }
        }
        for (k, v) in self.result.as_object().unwrap() {
            if actual.get(k).is_none() {
                println!("expect {}={}", k, v);
                return false;
            }
        }
        true
    }
}

impl PredicateReflection for ResultPredicate {}

impl Display for ResultPredicate {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.result.fmt(f)
    }
}

struct HomeDir {
    path: PathBuf,
    real_path: PathBuf,
    temp_home: tempfile::TempDir,
}

impl HomeDir {
    /// Rewrite paths to be relative to the temporary home directory
    fn rewrite_path(&self, path: &str) -> String {
        let path = PathBuf::from(path);
        if path.starts_with(&self.path) {
            #[allow(unused_mut)]
            let mut relative_path = path.strip_prefix(&self.path).unwrap().to_path_buf();
            #[cfg(target_os = "macos")]
            if relative_path.starts_with(".config") {
                relative_path = Path::new("Library")
                    .join("Application Support")
                    .join(relative_path.strip_prefix(".config").unwrap());
            }
            #[cfg(windows)]
            if relative_path.starts_with(".config/edgedb") {
                relative_path = Path::new("AppData")
                    .join("Local")
                    .join("EdgeDB")
                    .join("config")
                    .join(relative_path.strip_prefix(".config/edgedb").unwrap());
            }

            self.real_path
                .join(relative_path)
                .to_string_lossy()
                .to_string()
        } else {
            path.to_string_lossy().to_string()
        }
    }

    fn rewrite_dsn(&self, dsn: &str) -> String {
        let dsn = dsn.replace(
            self.path.to_str().unwrap(),
            self.real_path.to_str().unwrap(),
        );
        dsn
    }

    fn make_temp_file(&self, name: &str, content: &str) -> MockFile {
        let path = self
            .temp_home
            .path()
            .join(name)
            .to_str()
            .unwrap()
            .to_string();
        mock_file(&path, content)
    }
}

struct MockFile {
    path: PathBuf,
    is_dir: bool,
}

impl Drop for MockFile {
    fn drop(&mut self) {
        if !self.path.exists() {
            // this prevents abort on double-panic when test fails
            return;
        }
        if self.is_dir {
            fs::remove_dir(&self.path).unwrap_or_else(|_| panic!("rmdir {:?}", self.path));
        } else {
            fs::remove_file(&self.path).unwrap_or_else(|_| panic!("rm {:?}", self.path));
        }
    }
}

fn mock_file(path_str: &str, content: &str) -> MockFile {
    let path = PathBuf::from(path_str);
    if let Some(parent) = path.parent() {
        ensure_dir(parent, path_str);
    }
    fs::write(&path, content).unwrap_or_else(|e| panic!("Failed to write {:?}: {:?}", path, e));
    MockFile {
        path,
        is_dir: false,
    }
}

fn mock_project(
    project_dir: &str,
    project_path: &str,
    files: &indexmap::IndexMap<String, String>,
) -> Vec<MockFile> {
    let path = PathBuf::from(project_path);

    // Ensure the project path directory exists before canonicalizing
    if let Some(parent) = path.parent() {
        ensure_dir(parent, "project path");
    }

    // Create the path itself if it doesn't exist (for canonicalization)
    if !path.exists() {
        fs::create_dir_all(&path)
            .unwrap_or_else(|err| panic!("Failed to create path {:?}: {:?}", path, err));
    }

    let canon = dunce::canonicalize(&path).unwrap();
    let bytes = canon.as_os_str().as_encoded_bytes();
    let hash = hex::encode(sha1::Sha1::new_with_prefix(bytes).finalize());
    let project_dir = project_dir.replace("${HASH}", &hash);
    let project_dir = PathBuf::from(project_dir);
    let project_dir_mock = MockFile {
        path: project_dir.clone(),
        is_dir: true,
    };
    let project_path_file = mock_file(
        project_dir.join("project-path").to_str().unwrap(),
        project_path,
    );
    let link_file = project_dir.join("project-link");
    let is_dir;
    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_dir;
        symlink_dir(&path, &link_file).unwrap();
        is_dir = true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(&path, &link_file).unwrap();
        is_dir = false;
    }
    let mut rv = vec![
        project_path_file,
        MockFile {
            path: link_file,
            is_dir,
        },
    ];
    for (fname, data) in files {
        rv.push(mock_file(project_dir.join(fname).to_str().unwrap(), data));
    }
    rv.push(project_dir_mock);
    rv
}

fn ensure_dir(path: &Path, purpose: &str) {
    if !path.exists() {
        fs::create_dir_all(path)
            .unwrap_or_else(|err| panic!("{purpose}: mkdir -p {path:?}: {err:?}"));
    }
}

fn expect(result: Value) -> ResultPredicate {
    ResultPredicate { result }
}

fn run_test_case(case: &serde_json::Value) {
    let case = case.as_object().unwrap();
    let _name = case
        .get("name")
        .unwrap()
        .as_str()
        .unwrap()
        .replace(|c: char| !c.is_alphanumeric(), "_");

    let opts = case
        .get("opts")
        .and_then(|v| v.as_object())
        .filter(|m| !m.is_empty());
    let env = case
        .get("env")
        .and_then(|v| v.as_object())
        .filter(|m| !m.is_empty());
    let empty_map = serde_json::Map::new();
    let fs = case
        .get("fs")
        .and_then(|v| v.as_object())
        .unwrap_or(&empty_map);

    let result = case.get("result").map(|r| r.to_string());

    // Store mock files to keep them alive during the test
    let mut mock_files: Vec<MockFile> = Vec::new();
    let mut mock_projects: Vec<Vec<MockFile>> = Vec::new();

    // Always create a temporary home directory
    let temp_home = tempfile::Builder::new()
        .prefix("edgedb_test_home_")
        .tempdir()
        .unwrap();

    // Handle options
    let home = HomeDir {
        path: if let Some(homedir) = fs.get("homedir") {
            PathBuf::from(homedir.as_str().unwrap())
        } else {
            PathBuf::from("/home/edgedb")
        },
        #[cfg(windows)]
        real_path: std::env::home_dir().unwrap(),
        #[cfg(not(windows))]
        real_path: temp_home.path().to_path_buf(),
        temp_home,
    };

    let mut cmd = Command::cargo_bin("gel").unwrap_or_else(|_| Command::new("gel"));
    cmd.arg("--no-cli-update-check")
        .arg("--test-output-conn-params");

    if let Some(opts) = opts {
        for (key, value) in opts {
            match key.as_str() {
                "instance" => {
                    let argv = value.as_str().unwrap();
                    cmd.arg(format!("--instance={}", argv));
                }
                "credentialsFile" => {
                    if let Some(value) = value.as_str() {
                        cmd.arg("--credentials-file").arg(home.rewrite_path(value));
                    } else {
                        panic!("invalid credentialsFile value: {:?}", value);
                    }
                }
                "credentials" => {
                    if let Some(value) = value.as_str() {
                        let path = home.make_temp_file("credentials.json", value);
                        cmd.arg("--credentials-file").arg(&path.path);
                        mock_files.push(path);
                    } else {
                        panic!("invalid credentials value: {:?}", value);
                    }
                }
                "dsn" => {
                    if let Some(v) = value.as_str() {
                        let v = home.rewrite_dsn(v);
                        cmd.arg("--dsn").arg(v);
                    } else {
                        panic!("invalid dsn value: {:?}", value);
                    }
                }
                "host" => {
                    if let Some(v) = value.as_str() {
                        cmd.arg("--host").arg(v);
                    } else {
                        panic!("invalid host value: {:?}", value);
                    }
                }
                "port" => {
                    if let Some(v) = value.as_str() {
                        cmd.arg("--port").arg(v);
                    } else if let Some(v) = value.as_i64() {
                        cmd.arg("--port").arg(v.to_string());
                    } else if let Some(v) = value.as_f64() {
                        cmd.arg("--port").arg(v.to_string());
                    } else {
                        panic!("invalid port value: {:?}", value);
                    }
                }
                "database" => {
                    if let Some(v) = value.as_str() {
                        cmd.arg("--database").arg(v);
                    } else {
                        panic!("invalid database value: {:?}", value);
                    }
                }
                "branch" => {
                    if let Some(v) = value.as_str() {
                        cmd.arg("--branch").arg(v);
                    } else {
                        panic!("invalid branch value: {:?}", value);
                    }
                }
                "user" => {
                    if let Some(v) = value.as_str() {
                        cmd.arg("--user").arg(v);
                    } else {
                        panic!("invalid user value: {:?}", value);
                    }
                }
                "tlsSecurity" => {
                    if let Some(v) = value.as_str() {
                        cmd.arg("--tls-security").arg(v);
                    } else {
                        panic!("invalid tlsSecurity value: {:?}", value);
                    }
                }
                "tlsCA" => {
                    if let Some(value) = value.as_str() {
                        let path = home.make_temp_file("tls-ca.pem", value);
                        cmd.arg("--tls-ca-file").arg(&path.path);
                        mock_files.push(path);
                    } else {
                        panic!("invalid tlsCA value: {:?}", value);
                    }
                }
                "tlsCAFile" => {
                    if let Some(value) = value.as_str() {
                        cmd.arg("--tls-ca-file").arg(home.rewrite_path(value));
                    } else {
                        panic!("invalid tlsCAFile value: {:?}", value);
                    }
                }
                "tlsServerName" => {
                    if let Some(v) = value.as_str() {
                        cmd.arg("--tls-server-name").arg(v);
                    } else {
                        panic!("invalid tlsServerName value: {:?}", value);
                    }
                }
                "waitUntilAvailable" => {
                    if let Some(v) = value.as_str() {
                        cmd.arg("--wait-until-available").arg(v);
                    } else {
                        panic!("invalid waitUntilAvailable value: {:?}", value);
                    }
                }
                "secretKey" => {
                    if let Some(v) = value.as_str() {
                        cmd.arg("--secret-key").arg(v);
                    } else {
                        panic!("invalid secretKey value: {:?}", value);
                    }
                }
                "password" => {
                    if let Some(v) = value.as_str() {
                        let argv = format!("{}\n", v);
                        cmd.arg("--password-from-stdin").write_stdin(argv);
                    } else {
                        panic!("invalid password value: {:?}", value);
                    }
                }
                "serverSettings" => {
                    // Skip this test case
                    return;
                }
                _ => {
                    panic!("unknown opts key: {}", key);
                }
            }
        }
    }

    // Handle environment variables
    if let Some(env) = env {
        for (key, value) in env {
            let value = if let Some(v) = value.as_str() {
                v
            } else {
                panic!("invalid env {} value: {:?}", key, value);
            };
            if key.contains("_DSN") {
                cmd.env(key, home.rewrite_dsn(value));
            } else {
                cmd.env(key, home.rewrite_path(value));
            }
        }
    }

    // Redirect HOME and related environment variables so we can retarget away from the
    // real home directory.
    #[cfg(not(windows))]
    cmd.env("HOME", home.real_path.to_str().unwrap());
    #[cfg(target_os = "linux")]
    {
        cmd.env(
            "XDG_BIN_HOME",
            home.real_path.join(".local/bin").to_str().unwrap(),
        );
        cmd.env(
            "XDG_DATA_HOME",
            home.real_path.join(".local/share").to_str().unwrap(),
        );
        cmd.env(
            "XDG_CONFIG_HOME",
            home.real_path.join(".config").to_str().unwrap(),
        );
        cmd.env(
            "XDG_CACHE_HOME",
            home.real_path.join(".cache").to_str().unwrap(),
        );
    }

    // Handle filesystem setup
    if let Some(cwd) = fs.get("cwd") {
        let mut cwd = cwd.as_str().unwrap().to_string();
        cwd = home.rewrite_path(&cwd);
        ensure_dir(&PathBuf::from(&cwd), "cwd");
        cmd.current_dir(&cwd);
    }

    if let Some(files) = fs.get("files") {
        let files = files.as_object().unwrap();
        for (path, value) in files {
            let path = home.rewrite_path(path);
            if let Some(content) = value.as_str() {
                mock_files.push(mock_file(&path, content));
            } else if let Some(d) = value.as_object() {
                let mut d = d.clone();
                let project_path =
                    home.rewrite_path(d.remove("project-path").unwrap().as_str().unwrap());

                let mut project_files = indexmap::IndexMap::new();
                for (k, v) in d {
                    project_files.insert(k.to_string(), v.as_str().unwrap().to_string());
                }
                mock_projects.push(mock_project(&path, &project_path, &project_files));
            }
        }
    }

    // Run the command and check results
    if let Some(result) = &result {
        let result: Value = serde_json::from_str(result).unwrap();
        cmd.assert().success().stdout(expect(result));
    } else {
        let error_type = case
            .get("error")
            .and_then(|e| e.as_object())
            .and_then(|e| e.get("type"))
            .and_then(|e| e.as_str())
            .unwrap();

        let assertion = cmd.assert().failure();
        let output = assertion.get_output();
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Check if any of the patterns match
        for pattern in error_map().get(error_type).expect("unknown error type") {
            if predicates::str::is_match(pattern).unwrap().eval(&stderr) {
                let mut matched_patterns = MATCHED_PATTERNS.lock().unwrap();
                if matched_patterns.is_none() {
                    *matched_patterns = Some(HashSet::new());
                }
                if let Some(set) = matched_patterns.as_mut() {
                    set.insert(pattern);
                }
                return;
            }
        }

        panic!(
            "Expected error to match one of: {:?}\nActual stderr: {}",
            error_map().get(error_type).expect("unknown error type"),
            stderr
        );
    }
}

fn main() {
    // Get filter argument if provided
    let args: Vec<String> = std::env::args().collect();
    let filter = args.get(1).map(|s| s.as_str());

    // Load test cases from JSON file
    let testcases_path = std::env::var("CARGO_MANIFEST_DIR")
        .map(|dir| {
            PathBuf::from(dir)
                .join("..")
                .join("shared-client-testcases")
                .join("connection_testcases.json")
        })
        .unwrap_or_else(|_| PathBuf::from("../shared-client-testcases/connection_testcases.json"));

    let testcases_content = fs::read_to_string(&testcases_path)
        .expect("Shared test git submodule is missing: ensure git checkout includes submodules with --recurse-submodules");
    let testcases: Value = serde_json::from_str(&testcases_content).unwrap();

    let mut failed_tests = Vec::new();
    let mut total_tests = 0;
    let mut run_tests = 0;

    for case in testcases.as_array().unwrap() {
        let case = case.as_object().unwrap();
        let name = case.get("name").unwrap().as_str().unwrap();

        total_tests += 1;

        // Apply filter if provided
        if let Some(filter) = filter {
            if !name.to_lowercase().contains(&filter.to_lowercase()) {
                continue;
            }
        }

        // Skip platform-specific tests
        if let Some(platform) = case.get("platform").and_then(|p| p.as_str()) {
            match platform {
                "macos" if cfg!(not(target_os = "macos")) => continue,
                "windows" if cfg!(not(target_os = "windows")) => continue,
                "linux" if cfg!(not(target_os = "linux")) => continue,
                _ => {}
            }
        }

        // Skip tests that should panic
        if let Some(opts) = case.get("opts").and_then(|v| v.as_object()) {
            if let Some(dsn) = opts.get("dsn").and_then(|v| v.as_str()) {
                if dsn.contains("%25eth0") {
                    // servo/rust-url#424 - skip this test
                    continue;
                }
            }
        }

        run_tests += 1;
        print!("{} ... ", name);
        match std::panic::catch_unwind(|| run_test_case(&serde_json::Value::Object(case.clone()))) {
            Ok(_) => println!("✓ PASS"),
            Err(e) => {
                println!("✗: {e:?}");
                failed_tests.push(name.to_string());
            }
        }
    }

    println!("\nTest Summary:");
    println!("  Total tests available: {}", total_tests);
    println!("  Tests run: {}", run_tests);
    println!("  Tests passed: {}", run_tests - failed_tests.len());
    println!("  Tests failed: {}", failed_tests.len());

    if let Some(filter) = filter {
        println!("  Filter applied: '{}'", filter);
    } else if failed_tests.is_empty() {
        let matched_patterns = MATCHED_PATTERNS.lock().unwrap().take().unwrap();
        let mut unmatched_patterns = false;
        for (k, v) in error_map() {
            for pattern in v {
                if !matched_patterns.contains(pattern) {
                    println!("Unmatched error pattern: {}: {}", k, pattern);
                    unmatched_patterns = true;
                }
            }
        }

        if unmatched_patterns {
            println!("Unmatched error patterns found");
            std::process::exit(1);
        }
    }

    if !failed_tests.is_empty() {
        println!("\nFailed tests:");
        for test in failed_tests {
            println!("  - {}", test);
        }
        std::process::exit(1);
    }

    println!("\nAll tests passed!");
}
