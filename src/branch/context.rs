use gel_tokio::InstanceName;
use gel_tokio::dsn::DatabaseBranch;
use log::warn;

use crate::connect::Connection;
use crate::credentials;
use crate::platform::tmp_file_path;
use crate::portable::project::{self, get_stash_path};
use std::fs;
use std::sync::Mutex;

#[derive(Debug)]
pub struct Context {
    /// Instance name provided either with --instance or inferred from the project.
    instance_name: Option<InstanceName>,

    /// Project location if --instance was not specified and
    /// current directory is within a project.
    project: Option<project::Location>,

    /// None means that the current branch is unknown because:
    /// - instance is a cloud instance, which does not have a "current branch"
    ///   (when not used in a project)
    /// - the instance uses the default branch (and we cannot know what
    ///   that is without making a query), or
    /// - we don't know which instance we are connecting to. This might be because:
    ///   - there was neither a project or the --instance option,
    ///   - the project has no linked instance.
    ///
    ///   This happens when we supply just a URL, for example.
    current_branch: DatabaseBranch,

    /// Project manifest cache
    project_ctx_cache: Mutex<Option<project::Context>>,
}

impl Context {
    pub async fn new(instance_arg: Option<&InstanceName>) -> anyhow::Result<Context> {
        let mut ctx = Context {
            instance_name: None,
            current_branch: DatabaseBranch::Default,
            project: None,
            project_ctx_cache: Mutex::new(None),
        };

        // use instance name provided with --instance
        if let Some(instance_name) = instance_arg {
            ctx.instance_name = Some(instance_name.clone());

            match instance_name {
                InstanceName::Local(_) => {
                    // non-cloud instances have branch written in credentials.json
                    if let Some(credentials) = credentials::read(instance_name)? {
                        match (credentials.branch, credentials.database) {
                            (Some(branch), Some(_)) => {
                                ctx.current_branch = DatabaseBranch::Ambiguous(branch)
                            }
                            (Some(branch), None) => {
                                ctx.current_branch = DatabaseBranch::Branch(branch)
                            }
                            (None, Some(database)) => {
                                ctx.current_branch = DatabaseBranch::Database(database)
                            }
                            (None, None) => ctx.current_branch = DatabaseBranch::Default,
                        }
                    }
                }
                InstanceName::Cloud { .. } => {
                    // cloud instances do not have a current branch
                }
            }

            return Ok(ctx);
        }

        // find the project and use it's instance name and branch
        ctx.project = project::find_project_async(None).await?;
        if let Some(location) = &ctx.project {
            let stash_dir = get_stash_path(&location.root)?;
            ctx.instance_name = project::instance_name(&stash_dir).ok();
            ctx.current_branch =
                project::database_name(&stash_dir).unwrap_or(DatabaseBranch::Default);
        }

        Ok(ctx)
    }

    /// Returns the "current" branch or branch of the connection.
    /// Connection must not have its branch param modified.
    pub async fn get_current_branch(&self, connection: &mut Connection) -> anyhow::Result<String> {
        if let Some(b) = &self.current_branch.name() {
            return Ok(b.to_string());
        }

        // if the instance is unknown, current branch is just "the branch of the connection"
        // so we can pull it out here (if it is not the default branch)
        if let Some(name) = connection.database().name() {
            return Ok(name.to_string());
        }

        // if the connection branch is the default branch, query the database to see
        // what that default is
        Ok(connection.get_current_branch().await?.to_string())
    }

    pub fn can_update_current_branch(&self) -> bool {
        // we can update the current branch only if we know the instance, so we can write the credentials
        self.instance_name.is_some()
    }

    pub async fn update_current_branch(&self, branch: &str) -> anyhow::Result<()> {
        // If we are in a project, update the stash/database
        if let Some(project) = &self.project {
            let stash_path = get_stash_path(&project.root)?.join("database");

            // ensure that the temp file is created in the same directory as the 'database' file
            let tmp = tmp_file_path(&stash_path);
            fs::write(&tmp, branch)?;
            fs::rename(&tmp, &stash_path)?;
        }

        // If we have a local instance, also update the credentials.
        if let Some(x @ InstanceName::Local(_)) = &self.instance_name {
            if let Some(mut credentials) = credentials::read(x)? {
                credentials.database = Some(branch.to_string());
                credentials.branch = Some(branch.to_string());
                credentials::write(x, &credentials)?;
            } else {
                warn!("Credentials unexpectedly missing for {:#}", x);
            }
        }

        Ok(())
    }

    pub async fn get_project(&self) -> anyhow::Result<Option<project::Context>> {
        if let Some(ctx) = &*self.project_ctx_cache.lock().unwrap() {
            return Ok(Some(ctx.clone()));
        }

        let Some(location) = &self.project else {
            return Ok(None);
        };
        let ctx = project::load_ctx_at_async(location.clone()).await?;

        let mut cache_lock = self.project_ctx_cache.lock().unwrap();
        *cache_lock = Some(ctx.clone());
        Ok(Some(ctx))
    }
}
