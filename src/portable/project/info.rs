use std::fs;
use std::path::{Path, PathBuf};

use clap::ValueHint;
use const_format::concatcp;

use crate::branding::BRANDING_CLI_CMD;
use crate::branding::BRANDING_CLOUD;
use crate::commands::ExitCode;
use crate::portable::project;
use crate::print::{self, Highlight, msg};
use crate::table;

use super::get_stash_path;

pub fn run(options: &Command) -> anyhow::Result<()> {
    let ctx = project::ensure_ctx(options.project_dir.as_deref())?;
    let schema_dir = ctx.resolve_schema_dir()?;

    let stash_dir = get_stash_path(&ctx.location.root)?;
    if !stash_dir.exists() {
        msg!(
            "{} {} Run `{BRANDING_CLI_CMD} project init`.",
            print::err_marker(),
            "Project is not initialized.".emphasized()
        );
        return Err(ExitCode::new(1).into());
    }
    let instance_name = fs::read_to_string(stash_dir.join("instance-name"))?;
    let cloud_profile_file = stash_dir.join("cloud-profile");
    let cloud_profile = cloud_profile_file
        .exists()
        .then(|| fs::read_to_string(cloud_profile_file))
        .transpose()?;

    let item = options
        .get
        .as_deref()
        .or(options.instance_name.then_some("instance-name"));
    if let Some(item) = item {
        match item {
            "instance-name" => {
                if options.json {
                    println!("{}", serde_json::to_string(&instance_name)?);
                } else {
                    println!("{instance_name}");
                }
            }
            "cloud-profile" => {
                if options.json {
                    println!("{}", serde_json::to_string(&cloud_profile)?);
                } else if let Some(profile) = cloud_profile {
                    println!("{profile}");
                }
            }
            "schema-dir" => {
                if options.json {
                    println!("{}", serde_json::to_string(&schema_dir)?);
                } else {
                    println!("{}", schema_dir.display());
                }
            }
            _ => unreachable!(),
        }
    } else if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&JsonInfo {
                instance_name: &instance_name,
                cloud_profile: cloud_profile.as_deref(),
                root: &ctx.location.root,
                schema_dir: &schema_dir,
            })?
        );
    } else {
        let root = ctx.location.root.display().to_string();
        let schema_dir = schema_dir.display().to_string();

        let mut rows: Vec<(&str, String)> = vec![
            ("Instance name", instance_name),
            ("Project root", root),
            ("Schema dir", schema_dir),
        ];
        if let Some(profile) = cloud_profile.as_deref() {
            rows.push((concatcp!(BRANDING_CLOUD, " profile"), profile.to_string()));
        }
        table::settings(rows.as_slice());
    }
    Ok(())
}

#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    /// Explicitly set a root directory for the project
    #[arg(long, value_hint=ValueHint::DirPath)]
    pub project_dir: Option<PathBuf>,

    /// Display only the instance name (shortcut to `--get instance-name`)
    #[arg(long)]
    pub instance_name: bool,

    /// Output in JSON format
    #[arg(long)]
    pub json: bool,

    #[arg(long, value_parser=[
        "instance-name",
        "cloud-profile",
        "schema-dir",
    ])]
    /// Get a specific value:
    ///
    /// * `instance-name` -- Name of the listance the project is linked to
    pub get: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "kebab-case")]
struct JsonInfo<'a> {
    instance_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloud_profile: Option<&'a str>,
    root: &'a Path,
    schema_dir: &'a Path,
}
