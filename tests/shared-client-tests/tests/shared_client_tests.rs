use assert_cmd::Command;
use gel_protocol::model::Duration;
use predicates::reflection::PredicateReflection;
use predicates::Predicate;
use serde_json::Value;
use sha1::Digest;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

struct ResultPredicate {
    result: Value,
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

fn mock_file(path: &str, content: &str) -> MockFile {
    let path = PathBuf::from(path);
    ensure_dir(path.parent().unwrap());
    fs::write(&path, content).unwrap_or_else(|_| panic!("{}", "write {path:?}"));
    MockFile {
        path,
        is_dir: false,
    }
}

fn mock_project(
    project_dir: &str,
    project_path: &str,
    files: &indexmap::IndexMap<&str, &str>,
) -> Vec<MockFile> {
    let path = PathBuf::from(project_path);
    let canon = fs::canonicalize(&path).unwrap();
    #[cfg(windows)]
    let bytes = canon.to_str().unwrap().as_bytes();
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        canon.as_os_str().as_bytes()
    };
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

fn ensure_dir(path: &Path) {
    if !path.exists() {
        fs::create_dir_all(path).unwrap_or_else(|err| panic!("mkdir -p {:?}: {:?}", path, err));
    }
}

fn expect(result: Value) -> ResultPredicate {
    ResultPredicate { result }
}

include!(concat!(env!("OUT_DIR"), "/shared_client_testcases.rs"));
