use std::env;
use std::io::Write;
use std::process::Stdio;

use tempfile::NamedTempFile;
use tokio::process;

use crate::commands::options::Options;

#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    pub generator: String,
    pub arguments: Vec<String>,
}

pub fn prepare_command(cmd: &Command) -> Result<Vec<&str>, anyhow::Error> {
    let (lang, generator) = match cmd.generator.split_once("/") {
        Some(v) => v,
        None => anyhow::bail!("Generator should be of form <lang>/<tool>"),
    };

    let commands = if lang == "py" {
        if generator != "queries" {
            anyhow::bail!("Unknown Python generator {}", generator)
        }
        // XXX: or should we use `uv run`?
        vec!["gel-py"]
    } else if lang == "js" {
        vec!["npx", "@gel/generate", generator]
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
    let mut scmd = process::Command::new(cmdline[0]);
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

    let mut child = match scmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            anyhow::bail!("Error executing {}: {}", cmdline.join(" "), e,)
        }
    };

    let status = child.wait().await?;

    if !status.success() {
        anyhow::bail!("Child process {} failed with {}", cmdline.join(" "), status);
    }

    Ok(())
}
