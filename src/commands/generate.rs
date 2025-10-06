use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::Context;
use regex::Regex;
use tempfile::NamedTempFile;
use tokio::process;
use which::which;

use crate::branding::{BRANDING_CLI_CMD, MANIFEST_FILE_DISPLAY_NAME};
use crate::cli::env::{Env, UseUv};
use crate::commands::options::Options;
use crate::hint::HintExt;
use crate::print::{self, Highlight};
use crate::project;

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
    Ok(match env::var_os("VIRTUAL_ENV") {
        Some(env) => Some(env.into()),
        None => env::current_exe()?
            .parent()
            .and_then(Path::parent)
            .and_then(maybe_venv),
    })
}

fn maybe_venv(venv_root: impl AsRef<Path>) -> Option<PathBuf> {
    let venv_root = venv_root.as_ref();
    venv_root
        .join("pyvenv.cfg")
        .exists()
        .then(|| venv_root.into())
}

async fn detect_uv_project_root() -> anyhow::Result<Option<PathBuf>> {
    // Check if we should use uv
    match Env::use_uv()?.unwrap_or(UseUv::Auto) {
        UseUv::Never => return Ok(None),
        UseUv::Auto => {}
    }

    // Check if uv is installed
    let Ok(uv) = which("uv") else {
        return Ok(None);
    };

    // Run `uv version` to check if we are in a uv project.
    // We don't use `uv run --no-sync` here because it may still create `.venv`
    // and we don't want such side effects.
    let out = process::Command::new(&uv)
        .arg("version")
        // `--project <cwd>` is future-proofing against: https://github.com/astral-sh/uv/issues/11302
        .arg("--project")
        .arg(env::current_dir()?)
        // Make sure we can get the verbose output
        .env_remove("RUST_LOG")
        .arg("--verbose")
        // `--no-sync` is future-proofing against: https://github.com/astral-sh/uv/issues/14137
        .arg("--no-sync")
        .output()
        .await?;
    let stderr = std::str::from_utf8(&out.stderr)?;
    // If the command failed, it most likely means we are not in a uv project.
    // However, dynamic project version is a valid case, so we check for it.
    if !out.status.success()
        && !Regex::new("cannot get.*dynamic project version")
            .expect("valid regex")
            .is_match(stderr)
    {
        return Ok(None);
    }

    // Find the uv project root path, and return it.
    let workspace_regex = Regex::new("Found workspace root: `(.*)`").expect("valid regex");
    let project_regex = Regex::new("Found project root: `(.*)`").expect("valid regex");
    // Prefer workspace root over project root if both are present,
    // because the virtual environment is created under the workspace root.
    let Some(captures) = workspace_regex
        .captures(stderr)
        .or_else(|| project_regex.captures(stderr))
    else {
        return Err(anyhow::anyhow!(
            "Cannot find uv project root due to incompatible uv version"
        ))
        .hint("retry in a Python virtual environment; or diagnose with `uv version -v`")?;
    };
    Ok(Some(
        captures.get(1).expect("1st capture group").as_str().into(),
    ))
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

    let project = project::load_ctx(None, true).await?.ok_or_else(||
        anyhow::anyhow!(
            "`{MANIFEST_FILE_DISPLAY_NAME}` not found, unable to perform this action without an initialized project."
        )
    )?;
    let mut env = HashMap::new();
    if let Some(config) = project
        .manifest
        .generate
        .as_ref()
        .and_then(|d| d.get(lang))
        .and_then(|d| d.get(generator))
    {
        let mut generate_config = toml::map::Map::new();
        for (k, v) in config {
            let span = v.span();
            let span = [span.start, span.end]
                .iter()
                .map(|p| (*p).try_into().map(toml::Value::Integer))
                .collect::<Result<Vec<_>, _>>()?;
            let source = toml::map::Map::from_iter([
                ("span".into(), span.into()),
                ("manifest".into(), "project".into()),
            ]);
            let value = toml::map::Map::from_iter([
                ("source".into(), source.into()),
                ("value".into(), v.get_ref().clone()),
            ]);
            generate_config.insert(k.clone(), value.into());
        }
        let manifests = toml::map::Map::from_iter([(
            "project".into(),
            project.location.manifest.display().to_string().into(),
        )]);
        let manifest = toml::map::Map::from_iter([
            ("generate-config".into(), generate_config.into()),
            ("manifests".into(), manifests.into()),
        ]);
        env.insert(
            OsString::from("_GEL_MANIFEST"),
            OsString::from(serde_json::to_string(&manifest)?),
        );
        print::msg!(
            "Using generator configuration from project manifest: {}",
            serde_json::to_string_pretty(&config)?.emphasized(),
        );
    }

    let commands = if lang == "py" {
        #[cfg(not(windows))]
        let gen_name = "bin/gel-generate-py";
        #[cfg(windows)]
        let gen_name = "Scripts/gel-generate-py.exe";

        let mut venv = detect_venv()?;
        let mut using_uv = false;
        if venv.is_none() {
            if let Some(uv_proj) = detect_uv_project_root().await? {
                venv = maybe_venv(uv_proj.join(".venv"));
                if venv.is_none() {
                    return Err(anyhow::anyhow!(
                        "Cannot find virtual environment for uv project: {}",
                        uv_proj.display()
                    ))
                    .hint("retry after running `uv sync`")?;
                }
                using_uv = true;
            }
        }
        if let Some(venv) = venv {
            print::msg!(
                "Using Python virtual environment: {}",
                venv.display().to_string().emphasized()
            );
            let cmd = venv.join(gen_name);
            which(&cmd).map_err(|e| {
                anyhow::anyhow!(e)
                    .context(format!("cannot execute {}", cmd.display()))
                    .with_hint(|| {
                        if using_uv {
                            format!(
                                "run `uv add {GEL_PYTHON}` or `uv sync` to install the generator"
                            )
                        } else {
                            format!(
                                "make sure `{GEL_PYTHON}` is installed in the virtual environment"
                            )
                        }
                    })
            })?;
            vec![cmd.into(), generator.into()]
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
    write!(cred_file, "{json}")?;

    let (cmdline, extra_env) = prepare_command(cmd, subcommand_name).await?;
    let mut scmd = process::Command::new(cmdline[0].clone());
    scmd.args(&cmdline[1..])
        .args(cmd.arguments.iter().skip(1).cloned())
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
