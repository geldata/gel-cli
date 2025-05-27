use std::path::{Path, PathBuf};

use crate::migrations::apply::AutoBackup;
use crate::migrations::options::MigrationConfig;
use crate::portable::project::{self};

#[derive(Debug, Clone)]
pub struct Context {
    pub schema_dir: PathBuf,

    pub quiet: bool,
    pub skip_hooks: bool,

    pub project: Option<project::Context>,
    pub auto_backup: Option<AutoBackup>,
}

impl Context {
    pub async fn for_migration_config(
        cfg: &MigrationConfig,
        quiet: bool,
        skip_hooks: bool,
        read_only: bool,
    ) -> anyhow::Result<Context> {
        let project = project::load_ctx(None, read_only).await?;

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
            skip_hooks,
            auto_backup: None,
        })
    }

    pub fn for_project(project: project::Context, skip_hooks: bool) -> anyhow::Result<Context> {
        let schema_dir = project
            .manifest
            .project()
            .resolve_schema_dir(&project.location.root)?;

        Ok(Context {
            schema_dir,
            quiet: false,
            skip_hooks,
            project: Some(project),
            auto_backup: None,
        })
    }

    /// Create a context for a temporary path.
    ///
    /// Hooks are skipped.
    pub fn for_temp_path(path: impl AsRef<Path>) -> anyhow::Result<Context> {
        Ok(Context {
            schema_dir: path.as_ref().to_path_buf(),
            quiet: false,
            skip_hooks: true,
            project: None,
            auto_backup: None,
        })
    }

    pub fn with_auto_backup(self, auto_backup: Option<AutoBackup>) -> Self {
        Self {
            auto_backup,
            ..self
        }
    }
}
