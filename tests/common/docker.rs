//! Utils for invoking docker via spawning new processes.
#![allow(dead_code)]

use std::env;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use tar::{Builder, Header};

pub struct Context {
    tar: Builder<Vec<u8>>,
}

pub fn sudoers() -> &'static str {
    r###"
        root        ALL=(ALL:ALL) SETENV: ALL
        user1       ALL=(ALL:ALL) NOPASSWD: ALL
    "###
}

pub fn edbconnect_py() -> &'static str {
    r###"
import asyncio
import sys
import edgedb

assert edgedb.create_client(sys.argv[1]).query_single("SELECT 1+1") == 2

async def test_async():
    conn = edgedb.create_async_client(sys.argv[1])
    return await conn.query_single("SELECT 1+1")
assert asyncio.get_event_loop().run_until_complete(test_async()) == 2
    "###
}

pub fn edbconnect_js() -> &'static str {
    r###"
const edgedb = require('edgedb')
let client = edgedb.createClient(process.argv[2])
client.querySingle('SELECT 1+1')
    .then(function(value) {
        console.assert(value == 2, value)
        process.exit(0)
    })
    .catch(e => {
        console.error("Error", e)
        process.exit(1)
    })
    "###
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    pub fn new() -> Context {
        Context {
            tar: Builder::new(Vec::with_capacity(1048576)),
        }
    }
    pub fn add_file_mode(
        mut self,
        filename: impl AsRef<Path>,
        data: impl AsRef<[u8]>,
        mode: u32,
    ) -> anyhow::Result<Self> {
        let data = data.as_ref();
        let filename = filename.as_ref();
        let mut header = Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_path(filename)?;
        header.set_mode(mode);
        header.set_cksum();
        self.tar.append(&header, data)?;
        Ok(self)
    }
    pub fn add_dir(
        mut self,
        path: impl AsRef<Path>,
        src_path: impl AsRef<Path>,
    ) -> anyhow::Result<Self> {
        self.tar.append_dir_all(path, src_path)?;
        Ok(self)
    }
    pub fn add_file(
        self,
        filename: impl AsRef<Path>,
        data: impl AsRef<[u8]>,
    ) -> anyhow::Result<Self> {
        self.add_file_mode(filename, data, 0o644)
    }
    pub fn add_sudoers(self) -> anyhow::Result<Self> {
        self.add_file("sudoers", sudoers())
    }
    pub fn add_edbconnect(self) -> anyhow::Result<Self> {
        self.add_file("edbconnect.py", edbconnect_py())?
            .add_file("edbconnect.js", edbconnect_js())
    }
    pub fn add_bin(self) -> anyhow::Result<Self> {
        self.add_file_mode(
            "gel",
            fs::read(
                env::var("GEL_TEST_BIN_EXE").unwrap_or(env!("CARGO_BIN_EXE_gel").into()),
            )?,
            0o755,
        )
    }
    pub fn build(mut self) -> anyhow::Result<Vec<u8>> {
        self.tar.finish()?;
        Ok(self.tar.into_inner()?)
    }
}

pub fn build_image(context: Context, tagname: &str) -> anyhow::Result<()> {
    Command::new("docker")
        .arg("build")
        .arg("-")
        .arg("-t")
        .arg(tagname)
        .write_stdin(context.build()?)
        .assert()
        .success();
    Ok(())
}

pub fn run_bg(container_name: &str, tagname: &str) -> std::process::Child {
    std::process::Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--name")
        .arg(container_name)
        .arg(tagname)
        .spawn()
        .expect("can run docker command")
}

pub fn stop(container_name: &str) {
    Command::new("docker")
        .arg("stop")
        .arg(container_name)
        .assert();
}

pub fn run(tagname: &str, script: &str) -> assert_cmd::assert::Assert {
    let script = format!(
        r###"
        export EDGEDB_SKIP_DOCKER_CHECK=yes

        {script}
    "###,
        script = script
    );
    Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--mount=type=tmpfs,destination=/run/user/1000,tmpfs-mode=777")
        .arg("-u")
        .arg("1000")
        .arg(tagname)
        .args(["sh", "-exc", &script])
        .assert()
}

pub fn run_systemd(tagname: &str, script: &str) -> assert_cmd::assert::Assert {
    let script = format!(
        r###"
        export XDG_RUNTIME_DIR=/run/user/1000
        export RUST_LOG=info
        export EDGEDB_SKIP_DOCKER_CHECK=yes
        /lib/systemd/systemd --user --log-level=debug &

        {script}
    "###
    );
    let script = format!(
        r###"
        cg_path=$(cat /proc/self/cgroup | grep -oP '(?<=name=).*' | sed s/://)
        mkdir -p /run/user/1000 /sys/fs/cgroup/$cg_path
        chown user1 /sys/fs/cgroup/$cg_path /run/user/1000
        sudo -H -u user1 bash -exc {script}
    "###,
        script = shell_escape::escape(script.into())
    );
    Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("--tmpfs=/run")
        .arg("--tmpfs=/run/systemd/system")
        .arg("--privileged")
        .arg(tagname)
        .args(["sh", "-exc", &script])
        .assert()
}

pub fn run_with(tagname: &str, script: &str, link: &str) -> assert_cmd::assert::Assert {
    Command::new("docker")
        .arg("run")
        .arg("--rm")
        .arg("-u")
        .arg("1000")
        .arg(format!("--link={0}:{0}", link))
        .arg(tagname)
        .args(["sh", "-exc", script])
        .assert()
}

pub fn sudo_test(dockerfile: &str, tagname: &str, nightly: bool) -> Result<(), anyhow::Error> {
    let context = Context::new()
        .add_file("Dockerfile", dockerfile)?
        .add_sudoers()?
        .add_bin()?;
    build_image(context, tagname)?;
    run(
        tagname,
        &format!(
            r###"
                export RUST_LOG=info
                export EDGEDB_SKIP_DOCKER_CHECK=yes
                edgedb server install {arg}
                echo --- DONE ---
                /usr/bin/edgedb-server-* --version
            "###,
            arg = if nightly { "--nightly" } else { "" }
        ),
    )
    .success()
    .stdout(predicates::str::contains("--- DONE ---"))
    .stdout(predicates::function::function(|data: &str| {
        let tail = &data[data.find("--- DONE ---").unwrap()..];
        assert!(tail.contains("edgedb-server, version"));
        true
    }));
    Ok(())
}

pub fn install_twice_test(
    dockerfile: &str,
    tagname: &str,
    nightly: bool,
) -> Result<(), anyhow::Error> {
    let context = Context::new()
        .add_file("Dockerfile", dockerfile)?
        .add_sudoers()?
        .add_bin()?;
    build_image(context, tagname)?;
    run(
        tagname,
        &format!(
            r###"
                export EDGEDB_SKIP_DOCKER_CHECK=yes
                RUST_LOG=info edgedb server install {arg}
                echo --- DONE --- 1>&2
                RUST_LOG=info edgedb server install {arg}
            "###,
            arg = if nightly { "--nightly" } else { "" }
        ),
    )
    .code(51)
    .stderr(predicates::str::contains("--- DONE ---"))
    .stderr(predicates::function::function(|data: &str| {
        let tail = &data[data.find("--- DONE ---").unwrap()..];
        tail.contains("already downloaded")
    }));
    Ok(())
}
