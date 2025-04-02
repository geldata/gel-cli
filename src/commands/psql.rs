use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::branding::BRANDING;
use crate::connect::Connection;
use anyhow::Context;
use gel_tokio::server_params::{PostgresAddress, PostgresDsn};

use crate::commands::Options;
use crate::interrupt;
use crate::print;

pub async fn psql(cli: &mut Connection, _options: &Options) -> Result<(), anyhow::Error> {
    let mut cmd = Command::new("psql");
    let path = if let Some(dir) = option_env!("PSQL_DEFAULT_PATH") {
        let psql_path = Path::new(dir).join("psql");
        if !psql_path.exists() {
            eprintln!("WARNING: {} does not exist", psql_path.display());
        }
        let npath = if let Some(path) = env::var_os("PATH") {
            env::join_paths(std::iter::once(PathBuf::from(dir)).chain(env::split_paths(&path)))
                .unwrap_or_else(|e| {
                    eprintln!("PSQL_DEFAULT_PATH error: {e}");
                    path
                })
        } else {
            dir.into()
        };
        Some(npath)
    } else {
        env::var_os("PATH")
    };

    match cli.get_server_param::<PostgresAddress>() {
        Some(addr) => {
            cmd.arg("-h").arg(&addr.host);
            cmd.arg("-U").arg(&addr.user);
            cmd.arg("-p").arg(addr.port.to_string());
            cmd.arg("-d").arg(&addr.database);
        }
        None => match cli.get_server_param::<PostgresDsn>() {
            Some(addr) => {
                cmd.arg("--");
                cmd.arg(&addr.0);
            }
            None => {
                print::error!("{BRANDING} must be run in DEV mode to use psql.");
                return Ok(());
            }
        },
    }

    if let Some(path) = path.as_ref() {
        cmd.env("PATH", path);
    }

    let _trap = interrupt::Trap::new(&[interrupt::Signal::Interrupt]);
    cmd.status().with_context(|| {
        format!(
            "Error running {:?} (path: {:?})",
            cmd,
            path.unwrap_or_else(OsString::new)
        )
    })?;
    Ok(())
}
