use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::ValueHint;
use const_format::concatcp;
use gel_tokio::dsn::{DatabaseBranch, ProjectDir};

use crate::branding::BRANDING_CLI_CMD;
use crate::branding::BRANDING_CLOUD;
use crate::commands::ExitCode;
use crate::portable::project::manifest;
use crate::print::{self, Highlight, msg};
use crate::table;

pub fn run(options: &Command) -> anyhow::Result<()> {
    let dir = options
        .project_dir
        .clone()
        .map(ProjectDir::Search)
        .unwrap_or_else(|| ProjectDir::SearchCwd);
    let result = gel_tokio::dsn::ProjectSearchResult::find(dir)?
        .and_then(|m| m.project.map(|p| (p, m.project_path)));

    let Some((project, project_path)) = result else {
        msg!(
            "{} {} Run `{BRANDING_CLI_CMD} project init`.",
            print::err_marker(),
            "Project is not initialized.".emphasized()
        );
        return Err(ExitCode::new(1).into());
    };

    let mut data = BTreeMap::new();
    match project.db() {
        DatabaseBranch::Database(database) => {
            data.insert("database", database);
        }
        DatabaseBranch::Branch(branch) => {
            data.insert("branch", branch);
        }
        DatabaseBranch::Ambiguous(ambiguous) => {
            data.insert("branch", ambiguous);
        }
        DatabaseBranch::Default => {
            // If the project doesn't have a database, get it from the instance.
            let config = gel_tokio::dsn::Builder::new()
                .without_system()
                .with_fs()
                .with_auto_project_cwd()
                .build()?;
            match config.db {
                DatabaseBranch::Database(database) => {
                    data.insert("database", database);
                }
                DatabaseBranch::Branch(branch) => {
                    data.insert("branch", branch);
                }
                DatabaseBranch::Ambiguous(ambiguous) => {
                    data.insert("branch", ambiguous);
                }
                DatabaseBranch::Default => {
                    // Nobody has a db/branch setting, so we can't determine the database
                    data.insert("database", "(unavailable)".to_string());
                }
            }
        }
    }
    data.insert("instance-name", project.instance_name.to_string());
    if let Some(parent) = project_path.parent() {
        data.insert("root", parent.canonicalize()?.display().to_string());

        // TODO: this should be moved to gel-dsn
        let manifest = manifest::read(&project_path)?;
        let schema_dir = parent.join(manifest.project().get_schema_dir());
        data.insert("schema-dir", schema_dir.display().to_string());
    }
    if let Some(cloud_profile) = project.cloud_profile {
        data.insert("cloud-profile", cloud_profile);
    }

    let item = options
        .get
        .as_deref()
        .or(options.instance_name.then_some("instance-name"));

    if let Some(item) = item {
        let data = data
            .remove(item)
            .unwrap_or_else(|| "(unavailable)".to_string());
        if options.json {
            println!("{}", serde_json::to_string(&data)?);
        } else {
            println!("{data}");
        }
    } else if options.json {
        println!("{}", serde_json::to_string_pretty(&data)?);
    } else {
        let mut row_mapping: Vec<(&str, &str)> = vec![
            ("Branch", "branch"),
            ("Database", "database"),
            ("Instance name", "instance-name"),
            ("Project root", "root"),
            (concatcp!(BRANDING_CLOUD, " profile"), "cloud-profile"),
            ("Root", "root"),
            ("Schema directory", "schema-dir"),
        ];
        row_mapping.sort();

        let mut rows = Vec::new();
        for (friendly, internal) in row_mapping {
            if let Some(value) = data.remove(internal) {
                rows.push((friendly, value));
            }
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
        "branch",
        "database",
        "root",
    ])]
    /// Get a specific value:
    ///
    /// * `instance-name` -- Name of the listance the project is linked to
    pub get: Option<String>,
}
