use std::fs;
use std::io::{stdout, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use fn_error_context::context;

use crate::branding::BRANDING_CLI_CMD;
use crate::options::Options;
use crate::platform::{current_exe, home_dir};

#[derive(clap::Args, Clone, Debug)]
pub struct Command {
    /// Shell to print out completions for
    #[arg(long)]
    pub shell: Option<Shell>,

    /// Install all completions into the prefix
    #[arg(long, conflicts_with = "shell")]
    pub prefix: Option<PathBuf>,

    /// Install all completions into the prefix
    #[arg(long, conflicts_with = "shell", conflicts_with = "prefix")]
    pub home: bool,
}

pub fn run(cmd: &Command) -> anyhow::Result<()> {
    if let Some(shell) = cmd.shell {
        shell.generate(&mut stdout());
    } else if let Some(prefix) = &cmd.prefix {
        write_completion(
            &prefix.join("share/bash-completion/completions/edgedb"),
            Shell::Bash,
        )?;
        write_completion(
            &prefix.join("share/fish/completions/edgedb.fish"),
            Shell::Fish,
        )?;
        write_completion(&prefix.join("share/zsh/site-functions/_edgedb"), Shell::Zsh)?;
    } else if cmd.home {
        write_completions_home()?;
    } else {
        anyhow::bail!("either `--prefix` or `--shell=` is expected");
    }
    Ok(())
}

#[context("writing completion file {:?}", path)]
fn write_completion(path: &Path, shell: Shell) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    shell.generate(&mut BufWriter::new(fs::File::create(path)?));
    Ok(())
}

pub fn write_completions_home() -> anyhow::Result<()> {
    let home = home_dir()?;
    write_completion(
        &home.join(".local/share/bash-completion/completions/edgedb"),
        Shell::Bash,
    )?;
    write_completion(
        &home.join(".config/fish/completions/edgedb.fish"),
        Shell::Fish,
    )?;
    write_completion(&home.join(".zfunc/_edgedb"), Shell::Zsh)?;
    write_completion(
        &home.join(".local/share/bash-completion/completions/gel"),
        Shell::Bash,
    )?;
    write_completion(
        &home.join(".config/fish/completions/gel.fish"),
        Shell::Fish,
    )?;
    write_completion(&home.join(".zfunc/_gel"), Shell::Zsh)?;
    Ok(())
}
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
#[allow(clippy::enum_variant_names)]
pub enum Shell {
    Bash,
    Elvish,
    Fish,
    PowerShell,
    Zsh,
}

impl FromStr for Shell {
    type Err = anyhow::Error;
    fn from_str(v: &str) -> anyhow::Result<Shell> {
        use Shell::*;
        match v {
            "bash" => Ok(Bash),
            "elvish" => Ok(Elvish),
            "fish" => Ok(Fish),
            "powershell" => Ok(PowerShell),
            "zsh" => Ok(Zsh),
            _ => anyhow::bail!("unknown shell {:?}", v),
        }
    }
}

impl Shell {
    fn generate(&self, buf: &mut dyn Write) {
        use clap_complete::shells;

        let mut app = Options::command();

        let exe_path = current_exe().unwrap();
        let bin_name = exe_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap_or(BRANDING_CLI_CMD);
        match self {
            Shell::Bash => clap_complete::generate(shells::Bash, &mut app, bin_name, buf),
            Shell::Elvish => clap_complete::generate(shells::Elvish, &mut app, bin_name, buf),
            Shell::Fish => clap_complete::generate(shells::Fish, &mut app, bin_name, buf),
            Shell::Zsh => clap_complete::generate(shells::Zsh, &mut app, bin_name, buf),
            Shell::PowerShell => {
                clap_complete::generate(shells::PowerShell, &mut app, bin_name, buf)
            }
        }
    }
}
