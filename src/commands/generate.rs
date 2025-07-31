use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::{env, fs};

use anyhow::Context;
use tempfile::NamedTempFile;
use tokio::process;
use which::which;

use crate::branding::BRANDING_CLI_CMD;
use crate::cli::env::{Env, UseUv};
use crate::commands::options::Options;
use crate::hint::HintExt;
use crate::project;
use crate::print::{self, Highlight};

const GEL_PYTHON: &str = "gel";

#[derive(clap::Args, Debug, Clone)]
#[command(disable_help_flag = true)]
pub struct Command {
    /// Print help (see more with '--help')
    #[arg(short = 'h')]
    short_help: bool,

    /// Print help (see a summary with '-h')
    #[arg(long = "help")]
    long_help: bool,

    /// Arguments to pass to the generator.
    #[arg(trailing_var_arg = true)]
    pub arguments: Vec<String>,
}

fn print_help(short_help: bool, subcommand_name: &str) -> anyhow::Result<()> {
    let mut app = crate::options::Options::command().mut_subcommand(subcommand_name, |cmd| {
        cmd.arg(
            clap::arg!(<GENERATOR>)
                .value_parser([
                    "py/queries",
                    "py/models",
                    "js/edgeql-js",
                    "js/queries",
                    "js/interfaces",
                    "js/prisma",
                ])
                .help(color_print::cstr!(
                    "The generator to use, in the form <bold><<lang>>/<<tool>></bold>."
                ))
                .index(1),
        )
        .mut_args(|arg| match arg.get_id().as_str() {
            "arguments" => arg.index(2),
            "short_help" if short_help => arg.long("help"),
            "long_help" if !short_help => arg.short('h'),
            "short_help" | "long_help" => arg.short(None).long("hide").hide(true),
            _ => arg,
        })
    });
    let generate = app
        .find_subcommand_mut(subcommand_name)
        .expect("generate subcommand should exist");
    if short_help {
        generate.print_help()?;
    } else {
        generate.print_long_help()?;
    }
    Ok(())
}

fn detect_venv() -> anyhow::Result<Option<PathBuf>> {
    if let Some(env) = env::var_os("VIRTUAL_ENV") {
        Ok(Some(env.into()))
    } else {
        env::current_exe()?
            .parent()
            .and_then(|p| p.parent())
            .filter(|p| p.join("pyvenv.cfg").exists())
            .map(|p| Ok(p.into()))
            .transpose()
    }
}

async fn detect_uv() -> anyhow::Result<Option<PathBuf>> {
    match Env::use_uv()?.unwrap_or(UseUv::Auto) {
        UseUv::Never => return Ok(None),
        UseUv::Auto => {}
    }
    let Ok(uv) = which("uv") else {
        return Ok(None);
    };
    let out = process::Command::new(&uv)
        .arg("version")
        .arg("--output-format")
        .arg("json")
        .output()
        .await?;
    if out.status.success() {
        let package_name = serde_json::from_slice::<serde_json::Value>(&out.stdout)?
            .as_object()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "uv version output is not an object: {}",
                    String::from_utf8_lossy(&out.stdout)
                )
            })?
            .get("package_name")
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "uv version output does not contain package_name: {}",
                    String::from_utf8_lossy(&out.stdout)
                )
            })?
            .as_str()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "uv version output package_name is not a string: {}",
                    String::from_utf8_lossy(&out.stdout)
                )
            })?
            .emphasized();
        print::msg!("Detected uv project: {package_name}",);
        Ok(Some(uv))
    } else {
        Ok(None)
    }
}

pub async fn prepare_command(
    cmd: &Command,
    subcommand_name: &str,
) -> anyhow::Result<(Vec<OsString>, HashMap<OsString, OsString>)> {
    let (lang, generator) = cmd
        .arguments
        .first()
        .expect("generator argument is required")
        .split_once('/')
        .context("generator should be of form <lang>/<tool>")?;

    let project = project::ensure_ctx_async(None).await?;
    let mut env = HashMap::new();
    if let Some(config) = project
        .manifest
        .generate
        .as_ref()
        .and_then(|d| d.get(lang))
        .and_then(|d| d.get(generator))
    {
        let mut data = HashMap::new();
        for (k, v) in config {
            let span = v.span();
            let span = [span.start, span.end]
                .iter()
                .map(|p| (*p).try_into().map(toml::Value::Integer))
                .collect::<Result<Vec<_>, _>>()?;
            let mut value = HashMap::new();
            value.insert("span".to_string(), toml::Value::Array(span));
            value.insert("value".to_string(), v.get_ref().clone());
            data.insert(k.clone(), value);
        }
        env.insert(
            OsString::from("GEL_GENERATE_CONFIG"),
            OsString::from(serde_json::to_string(&data)?),
        );
        env.insert(
            OsString::from("GEL_TOML_CONTENT"),
            OsString::from(fs::read_to_string(project.location.manifest)?),
        );
        print::msg!(
            "Using generator configuration: {}",
            serde_json::to_string(&config)?.emphasized(),
        );
    }

    let commands = if lang == "py" {
        let gen_name = "gel-generate-py";

        if let Some(venv) = detect_venv()? {
            print::msg!(
                "Using Python virtual environment: {}",
                venv.display().to_string().emphasized()
            );
            let cmd = venv.join("bin").join(gen_name);
            which(&cmd).map_err(|e| {
                anyhow::anyhow!(e)
                    .context(format!("cannot execute {}", cmd.display()))
                    .with_hint(|| {
                        format!("have you installed `{GEL_PYTHON}` in the virtual environment?")
                    })
            })?;
            vec![cmd.into(), generator.into()]
        } else if let Some(uv) = detect_uv().await? {
            // We must not use `uv run gel-generate-py` here, because it may
            // use a global `gel-generate-py` command in $PATH, which is wrong.
            // Instead, we use `uv run` to run the current executable again
            // with UseUv::Never, so that it does not try to use uv again.
            UseUv::Never.set_env(|name, value| {
                env.insert(name.into(), value.into());
            });
            vec![
                uv.into(),
                "run".into(),
                env::current_exe()?.into(),
                subcommand_name.into(),
            ]
            .into_iter()
            .chain(cmd.arguments.iter().map(Into::into))
            .collect()
        } else {
            Err(anyhow::anyhow!("Cannot find environment to run the Python generator.")).with_hint(
                || format!(
                    "run `{BRANDING_CLI_CMD} {subcommand_name}` under your uv project subdirectory \
                    if you use uv, or in a Python virtual environment with `{GEL_PYTHON}` installed."
                )
            )?
        }
    } else if lang == "js" {
        vec!["npx", "@gel/generate", generator]
            .into_iter()
            .map(OsString::from)
            .collect()
    } else {
        anyhow::bail!("Unknown language {}", lang)
    };

    Ok((commands, env))
}

pub async fn run(
    cmd: &Command,
    options: &Options,
    subcommand_name: &str,
) -> Result<(), anyhow::Error> {
    if cmd.short_help || cmd.long_help || cmd.arguments.is_empty() {
        return print_help(cmd.short_help || !cmd.long_help, subcommand_name);
    }

    let creds = options.conn_params.get()?.as_credentials()?;
    let json = serde_json::to_string(&creds)?;
    let mut cred_file = NamedTempFile::new()?;
    write!(cred_file, "{}", json)?;

    let (cmdline, extra_env) = prepare_command(cmd, subcommand_name).await?;
    let mut scmd = process::Command::new(cmdline[0].clone());
    scmd.args(&cmdline[1..])
        .args(cmd.arguments.iter().skip(1).map(|s| s.clone()))
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
    for (name, value) in extra_env {
        scmd.env(name, value);
    }

    let cmdline_str = cmdline
        .into_iter()
        .map(|os| os.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");

    log::debug!("running `{cmdline_str}`");

    let mut child = match scmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            anyhow::bail!("Error executing {}: {}", cmdline_str, e,)
        }
    };

    let status = child.wait().await?;

    if !status.success() {
        anyhow::bail!("Child process {} failed with {}", cmdline_str, status);
    }

    Ok(())
}
