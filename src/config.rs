use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use fn_error_context::context;
use gel_protocol::model::Duration;

use crate::platform::config_dir;
use crate::repl;

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    #[serde(skip, default)]
    pub file_name: Option<PathBuf>,
    pub shell: ShellConfig,
}

#[derive(Debug, Clone, Default, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct ShellConfig {
    #[serde(default)]
    pub expand_strings: Option<bool>,
    #[serde(default)]
    pub history_size: Option<usize>,
    #[serde(default)]
    pub implicit_properties: Option<bool>,
    #[serde(default)]
    pub input_mode: Option<repl::InputMode>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default, deserialize_with = "parse_idle_tx_timeout")]
    pub idle_transaction_timeout: Option<Duration>,
    #[serde(default)]
    pub input_language: Option<repl::InputLanguage>,
    #[serde(default)]
    pub output_format: Option<repl::OutputFormat>,
    #[serde(default)]
    pub sql_output_format: Option<repl::OutputFormat>,
    #[serde(default)]
    pub display_typenames: Option<bool>,
    #[serde(default)]
    pub print_stats: Option<repl::PrintStats>,
    #[serde(default)]
    pub verbose_errors: Option<bool>,
}

pub fn get_config() -> anyhow::Result<Config> {
    let path = config_dir()?.join("cli.toml");
    if path.exists() {
        read_config(&path)
    } else {
        Ok(Default::default())
    }
}

#[context("reading file {:?}", path.as_ref())]
fn read_config(path: impl AsRef<Path>) -> anyhow::Result<Config> {
    let text = fs::read_to_string(&path)?;
    let toml = toml::de::Deserializer::new(&text);
    let mut val: Config = serde_path_to_error::deserialize(toml)?;
    val.file_name = Some(path.as_ref().to_path_buf());
    Ok(val)
}

fn parse_idle_tx_timeout<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: &str = serde::Deserialize::deserialize(deserializer)?;
    let rv = Duration::from_str(s).map_err(serde::de::Error::custom)?;

    // Postgres limits idle_in_transaction_session_timeout to non-negative i32.
    if rv.to_micros() < 0 {
        Err(serde::de::Error::custom("negative timeout is illegal"))
    } else if rv.to_micros() > 2147483647499 {
        Err(serde::de::Error::custom("timeout is too large"))
    } else {
        Ok(Some(rv))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    pub fn test_cli_config() {
        let tempdir = tempfile::tempdir().unwrap();
        let tempfile = tempdir.path().join("cli.toml");
        std::fs::write(&tempfile, "[shell]\n").unwrap();
        let config = read_config(tempfile).unwrap();
        assert_eq!(config.shell, ShellConfig::default());
    }

    #[test]
    pub fn test_doc_cli_config() {
        let tempdir = tempfile::tempdir().unwrap();
        let tempfile = tempdir.path().join("cli.toml");
        std::fs::write(
            &tempfile,
            br#"
[shell]
expand-strings = true
history-size = 10000
implicit-properties = false
limit = 100
input-mode = "emacs"
output-format = "json-pretty"
print-stats = "off"
verbose-errors = false
"#,
        )
        .unwrap();
        let config = read_config(tempfile).unwrap();
        assert_eq!(
            config.shell,
            ShellConfig {
                expand_strings: Some(true),
                history_size: Some(10000),
                implicit_properties: Some(false),
                limit: Some(100),
                idle_transaction_timeout: None,
                input_mode: Some(repl::InputMode::Emacs),
                output_format: Some(repl::OutputFormat::JsonPretty),
                sql_output_format: None,
                display_typenames: None,
                print_stats: Some(repl::PrintStats::Off),
                verbose_errors: Some(false),
                input_language: None,
            }
        );
    }
}
