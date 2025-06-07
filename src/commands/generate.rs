use std::env;
use std::ffi::OsString;
use std::io::Write;
use std::process::Stdio;

use anyhow::Context;
use tempfile::NamedTempFile;
use tokio::process;
use which::which;

use crate::commands::options::Options;

#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    pub generator: String,
    pub arguments: Vec<String>,
}

/// How we should invoke via `uv run` (or not).
#[derive(Debug, Clone, Copy)]
enum UseUv {
    Always,
    Auto,
    Never,
}

impl UseUv {
    /// Read from the `GEL_GENERATE_USE_UV` env var, defaulting to `auto`
    fn from_env() -> Self {
        match env::var("GEL_GENERATE_USE_UV")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "always" => UseUv::Always,
            "never" => UseUv::Never,
            _ => UseUv::Auto,
        }
    }
}

pub fn prepare_command(cmd: &Command) -> Result<Vec<OsString>, anyhow::Error> {
    let (lang, generator) = cmd
        .generator
        .split_once('/')
        .context("generator should be of form <lang>/<tool>")?;

    let use_uv = UseUv::from_env();

    let commands = if lang == "py" {
        let gen_name = "gel-generate-py";

        let uv_invocation = || -> Result<Vec<OsString>, anyhow::Error> {
            let uv = which("uv").context("`uv` not found on PATH")?;
            Ok(vec![
                uv.into_os_string(),
                OsString::from("run"),
                gen_name.into(),
                generator.into(),
            ])
        };

        match use_uv {
            UseUv::Always => {
                // always use uv
                uv_invocation()?
            }
            UseUv::Never => {
                // never use uv: must have the binary directly
                let direct =
                    which(gen_name).with_context(|| format!("`{}` not found on PATH", gen_name))?;
                vec![direct.into_os_string(), generator.into()]
            }
            UseUv::Auto => {
                // fallback logic: try direct, then uv
                if let Ok(direct) = which(gen_name) {
                    vec![direct.into_os_string(), generator.into()]
                } else {
                    // fall back to uv
                    uv_invocation()?
                }
            }
        }
    } else if lang == "js" {
        vec!["npx", "@gel/generate", generator]
            .into_iter()
            .map(OsString::from)
            .collect()
    } else {
        anyhow::bail!("Unknown language {}", lang)
    };

    Ok(commands)
}

pub async fn run(cmd: &Command, options: &Options) -> Result<(), anyhow::Error> {
    let creds = options.conn_params.get()?.as_credentials()?;
    let json = serde_json::to_string(&creds)?;
    let mut cred_file = NamedTempFile::new()?;
    write!(cred_file, "{}", json)?;

    let cmdline = prepare_command(cmd)?;
    let mut scmd = process::Command::new(cmdline[0].clone());
    scmd.args(&cmdline[1..])
        .args(cmd.arguments.clone())
        .stdin(Stdio::null())
        .stdout(Stdio::inherit());
    // Strip out all gel config env vars from the environment, since
    // everything should go via our GEL_CREDENTIALS_FILE.
    for (key, _) in env::vars() {
        if key.starts_with("GEL_") || key.starts_with("EDGEDB_") {
            scmd.env_remove(key);
        }
    }
    // Make GEL_CREDENTIALS_FILE our temp credentials.json file
    scmd.env("GEL_CREDENTIALS_FILE", cred_file.path().as_os_str());

    let cmdline_str = cmdline
        .into_iter()
        .map(|os| os.to_string_lossy().into_owned())
        .collect::<Vec<_>>().join(" ");

    log::debug!("running `{cmdline_str}`");

    let mut child = match scmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            anyhow::bail!("Error executing {}: {}", cmdline_str, e,)
        }
    };

    let status = child.wait().await?;

    if !status.success() {
        anyhow::bail!(
            "Child process {} failed with {}",
            cmdline_str,
            status
        );
    }

    Ok(())
}
