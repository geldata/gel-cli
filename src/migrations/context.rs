use std::path::PathBuf;

use crate::migrations::options::MigrationConfig;
use crate::portable::project;

pub struct Context {
    pub schema_dir: PathBuf,

    pub quiet: bool,

    pub project: Option<project::Context>,
}

impl Context {
    pub async fn for_migration_config(
        cfg: &MigrationConfig,
        quiet: bool,
    ) -> anyhow::Result<Context> {
        let project = project::load_ctx(None).await?;

        let schema_dir = if let Some(schema_dir) = &cfg.schema_dir {
            schema_dir.clone()
        } else if let Some(project) = &project {
            project.resolve_schema_dir()?
        } else {
            let default_dir: PathBuf = "./dbschema".into();
            if !default_dir.exists() {
                anyhow::bail!(
                    "`dbschema` directory doesn't exist. Either create one, init a project or provide its path via --schema-dir."
                );
            }
            default_dir
        };

        Ok(Context {
            schema_dir,
            quiet,
            project,
        })
    }
    pub fn for_project(project: project::Context) -> anyhow::Result<Context> {
        let schema_dir = project
            .manifest
            .project()
            .resolve_schema_dir(&project.location.root)?;
        Ok(Context {
            schema_dir,
            quiet: false,
            project: Some(project),
        })
    }
}
