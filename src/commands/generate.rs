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

fn print_help(short_help: bool) -> anyhow::Result<()> {
    let mut app = crate::options::Options::command().mut_subcommand("generate", |cmd| {
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
        .find_subcommand_mut("generate")
        .expect("generate subcommand should exist");
    if short_help {
        generate.print_help()?;
    } else {
        generate.print_long_help()?;
    }
    Ok(())
}

/// How we should invoke via `uvx` (or not).
#[derive(Debug, Clone, Copy)]
enum UseUvx {
    Always,
    Auto,
    Never,
}

impl UseUvx {
    /// Read from the `GEL_GENERATE_USE_UVX` env var, defaulting to `auto`
    fn from_env() -> Self {
        match env::var("GEL_GENERATE_USE_UVX")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "always" => UseUvx::Always,
            "never" => UseUvx::Never,
            _ => UseUvx::Auto,
        }
    }
}

pub fn prepare_command(cmd: &Command) -> Result<Vec<OsString>, anyhow::Error> {
    let (lang, generator) = cmd
        .arguments
        .first()
        .expect("generator argument is required")
        .split_once('/')
        .context("generator should be of form <lang>/<tool>")?;

    let use_uvx = UseUvx::from_env();

    let commands = if lang == "py" {
        let gen_name = "gel-generate-py";

        let uvx_invocation = || -> Result<Vec<OsString>, anyhow::Error> {
            let uvx = which("uvx").context("`uvx` not found on PATH")?;
            let mut gel = OsString::from("gel");
            gel.push(env::var_os("GEL_PYTHON_VERSION_SPEC").unwrap_or_else(|| ">=4.0.0b1".into()));
            Ok(vec![
                uvx.into_os_string(),
                OsString::from("--from"),
                gel,
                gen_name.into(),
                generator.into(),
            ])
        };

        match use_uvx {
            UseUvx::Always => {
                // always use uvx
                uvx_invocation()?
            }
            UseUvx::Never => {
                // never use uvx: must have the binary directly
                let direct =
                    which(gen_name).with_context(|| format!("`{}` not found on PATH", gen_name))?;
                vec![direct.into_os_string(), generator.into()]
            }
            UseUvx::Auto => {
                // fallback logic: try direct, then uvx
                if let Ok(direct) = which(gen_name) {
                    vec![direct.into_os_string(), generator.into()]
                } else {
                    // fall back to uvx
                    uvx_invocation()?
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
    if cmd.short_help || cmd.long_help || cmd.arguments.is_empty() {
        return print_help(cmd.short_help || !cmd.long_help);
    }

    let creds = options.conn_params.get()?.as_credentials()?;
    let json = serde_json::to_string(&creds)?;
    let mut cred_file = NamedTempFile::new()?;
    write!(cred_file, "{}", json)?;

    let cmdline = prepare_command(cmd)?;
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
