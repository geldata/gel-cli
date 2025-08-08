//# Project manifest

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use fn_error_context::context;

use toml::Spanned;

use crate::branding::MANIFEST_FILE_DISPLAY_NAME;
use crate::commands::ExitCode;
use crate::platform::tmp_file_path;
use crate::portable::exit_codes;
use crate::portable::repository::{Channel, Query};
use crate::print::{self, Highlight, msg};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Manifest {
    pub instance: Instance,
    pub project: Option<Project>,
    pub hooks: Option<Hooks>,
    pub hooks_extend: Option<Hooks>,
    pub watch: Vec<WatchScript>,
    pub generate: Option<BTreeMap<String, GenerateConfig>>,
}

impl Manifest {
    pub fn read_extended(self, path: &Path) -> anyhow::Result<Manifest> {
        let text = fs::read_to_string(path)?;
        let toml = toml::de::Deserializer::parse(&text)?;
        let ExtendManifest { hooks } = serde_path_to_error::deserialize(toml)?;
        Ok(Manifest {
            hooks_extend: hooks,
            ..self
        })
    }

    pub fn project(&self) -> Project {
        self.project.clone().unwrap_or_default()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Instance {
    #[serde(serialize_with = "serialize_query")]
    pub server_version: Query,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Project {
    pub schema_dir: Option<PathBuf>,
}

impl Project {
    pub fn get_schema_dir(&self) -> PathBuf {
        self.schema_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("dbschema"))
    }

    pub fn resolve_schema_dir(&self, root: &Path) -> anyhow::Result<PathBuf> {
        let schema_dir = root.join(self.get_schema_dir());

        if !schema_dir.exists() {
            return Ok(schema_dir);
        }

        dunce::canonicalize(&schema_dir)
            .with_context(|| format!("failed to canonicalize dir {schema_dir:?}"))
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Hooks {
    pub project: Option<ProjectHooks>,
    pub branch: Option<BranchHooks>,
    pub migration: Option<MigrationHooks>,
    pub schema: Option<SchemaHooks>,
    pub config: Option<ConfigHooks>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ProjectHooks {
    pub init: Option<Hook>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BranchHooks {
    pub switch: Option<Hook>,
    pub wipe: Option<Hook>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MigrationHooks {
    pub apply: Option<Hook>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SchemaHooks {
    pub update: Option<Hook>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ConfigHooks {
    pub update: Option<Hook>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Hook {
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct WatchScript {
    pub files: Vec<String>,
    pub script: String,
}

type GeneratorConfig = BTreeMap<String, Spanned<toml::Value>>;
type GenerateConfig = BTreeMap<String, GeneratorConfig>;

#[context("error reading project config `{}`", path.display())]
pub fn read(path: &Path) -> anyhow::Result<Manifest> {
    let text = fs::read_to_string(path)?;
    let toml = toml::de::Deserializer::parse(&text)?;
    let val: SrcManifest = serde_path_to_error::deserialize(toml)?;
    warn_extra(&val.extra, "");
    warn_extra(&val.instance.extra, "instance.");

    return Ok(Manifest {
        instance: Instance {
            server_version: val
                .instance
                .server_version
                .map(|x| x.into_inner())
                .unwrap_or(Query {
                    channel: Channel::Stable,
                    version: None,
                }),
        },
        project: Some(Project {
            schema_dir: val
                .project
                .and_then(|p| p.schema_dir)
                .map(|s| PathBuf::from(s.into_inner())),
        }),
        hooks: val.hooks,
        hooks_extend: None,
        watch: val.watch.unwrap_or_default(),
        generate: val.generate,
    });
}

#[context("cannot write config `{}`", path.display())]
pub fn write(path: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let text = toml::to_string(manifest).unwrap();

    let tmp = tmp_file_path(path);
    fs::remove_file(&tmp).ok();
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Modify a field in a config of type `Cfg` that was deserialized from `input`.
/// The field is selected with the `selector` function.
fn modify<Cfg, Selector, Val, ToStr>(
    parsed: &Cfg,
    input: &str,
    selector: Selector,
    field_name: &'static str,
    value: &Val,
    to_str: ToStr,
) -> anyhow::Result<Option<String>>
where
    Selector: Fn(&Cfg) -> &Option<Spanned<Val>>,
    Val: std::cmp::PartialEq,
    ToStr: FnOnce(&Val) -> String,
{
    use std::fmt::Write;

    if let Some(selected) = selector(parsed) {
        if selected.get_ref() == value {
            return Ok(None);
        }

        let mut out = String::with_capacity(input.len() + 5);
        write!(
            &mut out,
            "{}{:?}{}",
            &input[..selected.span().start],
            to_str(value),
            &input[selected.span().end..]
        )
        .unwrap();

        return Ok(Some(out));
    }

    print::error!("Invalid {MANIFEST_FILE_DISPLAY_NAME}: missing {field_name}");
    Err(ExitCode::new(exit_codes::INVALID_CONFIG).into())
}

fn read_modify_write<Cfg, Selector, Val, ToStr>(
    path: &Path,
    selector: Selector,
    field_name: &'static str,
    value: &Val,
    to_str: ToStr,
) -> anyhow::Result<bool>
where
    Cfg: for<'de> serde::Deserialize<'de>,
    Selector: Fn(&Cfg) -> &Option<Spanned<Val>>,
    Val: std::cmp::PartialEq,
    ToStr: FnOnce(&Val) -> String,
{
    let input = fs::read_to_string(path)?;
    let deserializer = toml::de::Deserializer::parse(&input)?;
    let parsed: Cfg = serde_path_to_error::deserialize(deserializer)?;

    if let Some(new_contents) = modify(&parsed, &input, selector, field_name, value, to_str)? {
        let tmp = tmp_file_path(path);
        fs::remove_file(&tmp).ok();
        fs::write(&tmp, new_contents)?;
        fs::rename(&tmp, path)?;

        Ok(true)
    } else {
        Ok(false)
    }
}

#[context("cannot modify `{}`", config.display())]
pub fn modify_server_ver(config: &Path, ver: &Query) -> anyhow::Result<bool> {
    msg!(
        "Setting `server-version = {}` in `{}`",
        format_args!("{:?}", ver.as_config_value())
            .to_string()
            .emphasized(),
        config.file_name().unwrap_or_default().to_string_lossy()
    );
    read_modify_write(
        config,
        |v: &SrcManifest| &v.instance.server_version,
        "server-version",
        ver,
        Query::as_config_value,
    )
}

fn warn_extra(extra: &BTreeMap<String, toml::Value>, prefix: &str) {
    for key in extra.keys() {
        log::warn!("Unknown config option `{}{}`", prefix, key.escape_default());
    }
}

fn serialize_query<S>(query: &Query, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    s.serialize_str(&query.as_config_value())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SrcManifest {
    #[serde(alias = "edgedb")]
    pub instance: SrcInstance,
    pub project: Option<SrcProject>,
    pub hooks: Option<Hooks>,
    pub watch: Option<Vec<WatchScript>>,
    pub generate: Option<BTreeMap<String, GenerateConfig>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ExtendManifest {
    #[serde(alias = "hooks-extend")]
    pub hooks: Option<Hooks>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SrcInstance {
    #[serde(default)]
    pub server_version: Option<toml::Spanned<Query>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SrcProject {
    #[serde(default)]
    pub schema_dir: Option<toml::Spanned<String>>,
    #[serde(flatten)]
    #[allow(dead_code)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[cfg(test)]
mod test {
    use test_case::test_case;

    const TOML_BETA1: &str = "\
        [edgedb]\n\
        server-version = \"1.0-beta.1\"\n\
    ";
    const TOML_BETA2: &str = "\
        [edgedb]\n\
        server-version = \"1.0-beta.2\"\n\
    ";
    const TOML_BETA2_CUSTOM_SCHEMA_DIR: &str = "\
        [edgedb]\n\
        server-version = \"1.0-beta.2\"\n\
        [project]
        schema-dir = \"custom-dir\"\n\
    ";
    const TOML_2_3: &str = "\
        [edgedb]\n\
        server-version = \"2.3\"\n\
    ";
    const TOML_2_3_EXACT: &str = "\
        [edgedb]\n\
        server-version = \"=2.3\"\n\
    ";
    const TOML_NIGHTLY: &str = "\
        [edgedb]\n\
        server-version = \"nightly\"\n\
    ";
    const TOML_NIGHTLY_CUSTOM_SCHEMA_DIR: &str = "\
        [edgedb]\n\
        server-version = \"nightly\"\n\
        [project]
        schema-dir = \"custom-dir\"\n\
    ";

    const TOML2_BETA1: &str = "\
        [edgedb]\n\
        # some comment\n\
        server-version = \"1.0-beta.1\" #and here\n\
        other-setting = true\n\
    ";
    const TOML2_BETA2: &str = "\
        [edgedb]\n\
        # some comment\n\
        server-version = \"1.0-beta.2\" #and here\n\
        other-setting = true\n\
    ";
    const TOML2_BETA2_CUSTOM_SCHEMA_DIR: &str = "\
        [edgedb]\n\
        # some comment\n\
        server-version = \"1.0-beta.2\" #and here\n\
        other-setting = true\n\
        [project]
        schema-dir = \"custom-dir\"\n\
    ";
    const TOML2_NIGHTLY: &str = "\
        [edgedb]\n\
        # some comment\n\
        server-version = \"nightly\" #and here\n\
        other-setting = true\n\
    ";
    const TOML2_NIGHTLY_CUSTOM_SCHEMA_DIR: &str = "\
        [edgedb]\n\
        # some comment\n\
        server-version = \"nightly\" #and here\n\
        other-setting = true\n\
        [project]
        schema-dir = \"custom-dir\"\n\
    ";

    const TOMLI_BETA1: &str = "\
        edgedb = {server-version = \"1.0-beta.1\"}\n\
    ";
    const TOMLI_BETA2: &str = "\
        edgedb = {server-version = \"1.0-beta.2\"}\n\
    ";
    const TOMLI_BETA2_CUSTOM_SCHEMA_DIR: &str = "\
        edgedb = {server-version = \"1.0-beta.2\"}\n\
        project = {schema-dir = \"custom-dir\"}\n\
    ";
    const TOMLI_NIGHTLY: &str = "\
        edgedb = {server-version = \"nightly\"}\n\
    ";
    const TOMLI_NIGHTLY_CUSTOM_SCHEMA_DIR: &str = "\
        edgedb = {server-version = \"nightly\"}\n\
        project = {schema-dir = \"custom-dir\"}\n\
    ";
    const TOMLI_DEV: &str = "\
        edgedb = {server-version = \"6.0-dev.8321\"}\n\
    ";

    fn set_toml_version(data: &str, version: &super::Query) -> anyhow::Result<Option<String>> {
        let toml = toml::de::Deserializer::parse(data)?;
        let parsed: super::SrcManifest = serde_path_to_error::deserialize(toml)?;

        super::modify(
            &parsed,
            data,
            |v: &super::SrcManifest| &v.instance.server_version,
            "server-version",
            version,
            super::Query::as_config_value,
        )
    }

    #[test_case(TOML_BETA1, "1.0-beta.2" => Some(TOML_BETA2.into()))]
    #[test_case(TOML_BETA2, "1.0-beta.2" => None)]
    #[test_case(TOML_NIGHTLY, "1.0-beta.2" => Some(TOML_BETA2.into()))]
    #[test_case(TOML_BETA1, "1.0-beta.1" => None)]
    #[test_case(TOML_BETA2, "1.0-beta.1" => Some(TOML_BETA1.into()))]
    #[test_case(TOML_NIGHTLY, "1.0-beta.1" => Some(TOML_BETA1.into()))]
    #[test_case(TOML_BETA1, "nightly" => Some(TOML_NIGHTLY.into()))]
    #[test_case(TOML_BETA2, "nightly" => Some(TOML_NIGHTLY.into()))]
    #[test_case(TOML_BETA2_CUSTOM_SCHEMA_DIR, "nightly" => Some(TOML_NIGHTLY_CUSTOM_SCHEMA_DIR.into()))]
    #[test_case(TOML_NIGHTLY, "nightly" => None)]
    #[test_case(TOML_2_3, "=2.3" => Some(TOML_2_3_EXACT.into()))]
    #[test_case(TOML2_BETA1, "1.0-beta.2" => Some(TOML2_BETA2.into()))]
    #[test_case(TOML2_BETA2, "1.0-beta.2" => None)]
    #[test_case(TOML2_NIGHTLY, "1.0-beta.2" => Some(TOML2_BETA2.into()))]
    #[test_case(TOML2_BETA1, "1.0-beta.1" => None)]
    #[test_case(TOML2_BETA2, "1.0-beta.1" => Some(TOML2_BETA1.into()))]
    #[test_case(TOML2_NIGHTLY, "1.0-beta.1" => Some(TOML2_BETA1.into()))]
    #[test_case(TOML2_BETA1, "nightly" => Some(TOML2_NIGHTLY.into()))]
    #[test_case(TOML2_BETA2, "nightly" => Some(TOML2_NIGHTLY.into()))]
    #[test_case(TOML2_BETA2_CUSTOM_SCHEMA_DIR, "nightly" => Some(TOML2_NIGHTLY_CUSTOM_SCHEMA_DIR.into()))]
    #[test_case(TOML2_NIGHTLY, "nightly" => None)]
    #[test_case(TOMLI_BETA1, "1.0-beta.2" => Some(TOMLI_BETA2.into()))]
    #[test_case(TOMLI_BETA2, "1.0-beta.2" => None)]
    #[test_case(TOMLI_NIGHTLY, "1.0-beta.2" => Some(TOMLI_BETA2.into()))]
    #[test_case(TOMLI_BETA1, "1.0-beta.1" => None)]
    #[test_case(TOMLI_BETA2, "1.0-beta.1" => Some(TOMLI_BETA1.into()))]
    #[test_case(TOMLI_NIGHTLY, "1.0-beta.1" => Some(TOMLI_BETA1.into()))]
    #[test_case(TOMLI_BETA1, "nightly" => Some(TOMLI_NIGHTLY.into()))]
    #[test_case(TOMLI_BETA2, "nightly" => Some(TOMLI_NIGHTLY.into()))]
    #[test_case(TOMLI_BETA2_CUSTOM_SCHEMA_DIR, "nightly"=> Some(TOMLI_NIGHTLY_CUSTOM_SCHEMA_DIR.into()))]
    #[test_case(TOMLI_NIGHTLY, "nightly" => None)]
    #[test_case(TOMLI_BETA1, "6.0-dev.8321" => Some(TOMLI_DEV.into()))]
    #[test_case(TOMLI_DEV, "1.0-beta.1" => Some(TOMLI_BETA1.into()))]
    #[test_case(TOMLI_DEV, "nightly" => Some(TOMLI_NIGHTLY.into()))]
    fn modify(src: &str, ver: &str) -> Option<String> {
        set_toml_version(src, &ver.parse().unwrap()).unwrap()
    }
}
