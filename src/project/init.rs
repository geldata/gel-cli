use std::env;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::Context;
use clap::ValueHint;
use const_format::concatcp;
use gel_cli_instance::cloud::CloudInstanceCreate;
use gel_tokio::dsn::{DEFAULT_BRANCH_NAME_CREATE, DEFAULT_DATABASE_NAME, DatabaseBranch};
use gel_tokio::{CloudName, InstanceName, PROJECT_FILES};
use rand::{Rng, thread_rng};

use edgeql_parser::helpers::quote_name;
use gel_errors::DuplicateDatabaseDefinitionError;

use crate::branding::BRANDING_CLOUD;
use crate::branding::QUERY_TAG;
use crate::branding::{BRANDING, BRANDING_CLI_CMD, MANIFEST_FILE_DISPLAY_NAME};
use crate::cloud::client::CloudClient;
use crate::commands::ExitCode;
use crate::connect::Connection;
use crate::connect::Connector;
use crate::credentials;
use crate::instance::control;
use crate::instance::create;
use crate::migrations;
use crate::options::CloudOptions;
use crate::portable::exit_codes;
use crate::portable::local::{InstanceInfo, Paths, allocate_port};
use crate::portable::options::{CloudInstanceBillables, CloudInstanceParams};
use crate::portable::platform::optional_docker_check;
use crate::portable::repository::{self, Channel, PackageInfo, Query};
use crate::portable::server::install;
use crate::portable::ver;
use crate::portable::ver::Specific;
use crate::portable::windows;
use crate::print::{self, Highlight, msg};
use crate::project::{self, get_stash_path};
use crate::question;
use crate::table;
use crate::{cloud, hooks};

#[allow(clippy::collapsible_else_if)]
pub fn run(options: &Command, opts: &crate::options::Options) -> anyhow::Result<()> {
    if optional_docker_check()? {
        print::error!("`{BRANDING_CLI_CMD} project init` is not supported in Docker containers.");
        Err(ExitCode::new(exit_codes::DOCKER_CONTAINER))?;
    }

    if options.server_start_conf.is_some() {
        print::warn!(
            "The option `--server-start-conf` is deprecated. \
                     Use `{BRANDING_CLI_CMD} instance start/stop` to control \
                     the instance."
        );
    }

    let project_loc = project::find_project(options.project_dir.as_deref())?;

    if let Some(project_loc) = project_loc {
        if options.link {
            link(options, project_loc, opts)?;
        } else {
            init_existing(options, project_loc, opts)?;
        }
    } else {
        if options.link {
            anyhow::bail!(
                "{MANIFEST_FILE_DISPLAY_NAME} not found, unable to link an existing {BRANDING} \
                instance without an initialized project. To initialize \
                a project, run `{BRANDING_CLI_CMD}` command without `--link` flag"
            )
        } else {
            let root = options
                .project_dir
                .clone()
                .unwrap_or_else(|| env::current_dir().unwrap());
            let manifest = root.join(if cfg!(feature = "gel") {
                PROJECT_FILES[0]
            } else {
                PROJECT_FILES[1]
            });
            let location = project::Location { root, manifest };
            init_new(options, location, opts)?;
        }
    };

    Ok(())
}

#[derive(clap::Args, Debug, Clone)]
pub struct Command {
    #[command(flatten)]
    pub cloud_opts: CloudOptions,

    /// Explicitly set a root directory for the project
    #[arg(long, value_hint=ValueHint::DirPath)]
    pub project_dir: Option<PathBuf>,

    /// Specify the desired EdgeDB server version
    #[arg(long)]
    pub server_version: Option<Query>,

    /// Specify whether the existing EdgeDB server instance
    /// should be linked with the project
    #[arg(long)]
    pub link: bool,

    /// Specify the EdgeDB server instance to be associated with the project
    #[arg(long)]
    pub server_instance: Option<InstanceName>,

    /// Specify the default database for the project to use on that instance
    #[arg(long, short = 'd')]
    pub database: Option<String>,

    /// Specify the default branch for the project to use on that instance
    #[arg(long, short = 'b')]
    pub branch: Option<String>,

    /// Deprecated parameter, does nothing.
    #[arg(long, hide = true)]
    pub server_start_conf: Option<create::StartConf>,

    /// Skip running migrations
    ///
    /// There are two main use cases for this option:
    /// 1. With `--link` to connect to a datastore with existing data
    /// 2. To initialize a new instance but then restore using a dump
    #[arg(long)]
    pub no_migrations: bool,

    /// Initialize in non-interactive mode (accepting all defaults)
    #[arg(long, hide = true)]
    pub non_interactive: bool,

    /// Initialize in interactive mode
    #[arg(long)]
    pub interactive: bool,
}

impl Command {
    /// Compatibility with older --database flag
    pub fn database(&self) -> &Option<String> {
        if self.branch.is_some() {
            &self.branch
        } else {
            &self.database
        }
    }
}

pub fn init_existing(
    cmd: &Command,
    project: project::Location,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    msg!(
        "Found `{}` in {}",
        project
            .manifest
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        project.root.display()
    );
    let stash_dir = get_stash_path(&project.root)?;
    if stash_dir.exists() {
        // TODO(tailhook) do more checks and probably cleanup the dir
        anyhow::bail!("Project is already initialized.");
    }
    msg!("Initializing project...");

    let project = project::load_ctx_at(project)?;
    let schema_dir = project
        .manifest
        .project()
        .resolve_schema_dir(&project.location.root)?;
    let schema_files = project::find_schema_files(&schema_dir)?;

    let ver_query = if let Some(sver) = &cmd.server_version {
        sver.clone()
    } else {
        project.manifest.instance.server_version.clone()
    };
    let mut client = CloudClient::new(&opts.cloud_options)?;
    let (name, exists) = ask_name(&project.location.root, cmd, &mut client)?;

    if exists {
        let mut inst = project::Handle::probe(&name, &project.location.root, &schema_dir, &client)?;
        let specific_version: &Specific = &inst.get_version()?.specific();
        inst.check_version(&ver_query);

        if matches!(name, InstanceName::Cloud { .. }) {
            if !cmd.interactive {
                inst.database = Some(
                    cmd.database()
                        .clone()
                        .unwrap_or(get_default_branch_or_database(specific_version)),
                );
            } else {
                inst.database = ask_database_or_branch(specific_version)?
                    .name()
                    .map(|s| s.to_string());
            }
        } else {
            inst.database.clone_from(cmd.database());
        }
        return do_link(&inst, &project, cmd, &stash_dir, opts);
    }

    match &name {
        InstanceName::Cloud(name) => {
            msg!("Checking {BRANDING_CLOUD} versions...");

            let ver = cloud::versions::get_version(&ver_query, &client)
                .with_context(|| "could not initialize project")?;
            ver::print_version_hint(&ver, &ver_query);
            let database = ask_database()?;

            table::settings(&[
                (
                    "Project directory",
                    project.location.root.display().to_string(),
                ),
                (
                    "Project config",
                    project.location.manifest.display().to_string(),
                ),
                (
                    &format!(
                        "Schema dir {}",
                        if schema_files {
                            "(non-empty)"
                        } else {
                            "(empty)"
                        }
                    ),
                    schema_dir.display().to_string(),
                ),
                (
                    if ver.major >= 5 {
                        "Branch name"
                    } else {
                        "Database name"
                    },
                    database.to_string(),
                ),
                ("Version", ver.to_string()),
                ("Instance name", name.to_string()),
            ]);

            if !schema_files {
                project::write_schema_default(&schema_dir, &Query::from_version(&ver)?)?;
            }
            do_cloud_init(
                name.clone(),
                &stash_dir,
                &project,
                &ver,
                database,
                cmd,
                opts,
                &client,
            )
        }
        InstanceName::Local(name) => {
            msg!("Checking {BRANDING} versions...");

            let pkg = repository::get_server_package(&ver_query)?.with_context(|| {
                format!(
                    "cannot find package matching {}. \
                    (Use `{BRANDING_CLI_CMD} server list-versions` to see all available)",
                    ver_query.display()
                )
            })?;
            let specific_version = &pkg.version.specific();
            ver::print_version_hint(specific_version, &ver_query);

            let mut branch = DatabaseBranch::default();
            if cmd.interactive && specific_version.major >= 5 {
                branch = ask_branch()?;
            }

            let meth = if cfg!(windows) {
                "WSL".to_string()
            } else {
                "portable package".to_string()
            };

            let schema_dir_key = &format!(
                "Schema dir {}",
                if schema_files {
                    "(non-empty)"
                } else {
                    "(empty)"
                }
            );

            let mut rows: Vec<(&str, String)> = vec![
                (
                    "Project directory",
                    project.location.root.display().to_string(),
                ),
                (
                    "Project config",
                    project.location.manifest.display().to_string(),
                ),
                (schema_dir_key, schema_dir.display().to_string()),
                ("Installation method", meth),
                ("Version", pkg.version.to_string()),
                ("Instance name", name.clone()),
            ];

            if let Some(branch) = branch.branch_for_create() {
                rows.push(("Branch", branch.to_string()))
            }

            table::settings(rows.as_slice());

            if !schema_files {
                project::write_schema_default(
                    &schema_dir,
                    &Query::from_version(specific_version)?,
                )?;
            }

            do_init(name, &pkg, &stash_dir, &project, branch, cmd, opts)
        }
    }
}

fn do_init(
    name: &str,
    pkg: &PackageInfo,
    stash_dir: &Path,
    project: &project::Context,
    database: DatabaseBranch,
    cmd: &Command,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    let port = allocate_port(name)?;
    let paths = Paths::get(name)?;
    let inst_name = InstanceName::Local(name.to_owned());

    let instance = if cfg!(windows) {
        let q = repository::Query::from_version(&pkg.version.specific())?;
        windows::create_instance(
            &create::Command {
                name: Some(inst_name.clone()),
                instance: None,
                nightly: false,
                channel: q.cli_channel(),
                version: q.version,
                cloud_params: CloudInstanceParams {
                    region: None,
                    billables: CloudInstanceBillables {
                        tier: None,
                        compute_size: None,
                        storage_size: None,
                    },
                },
                cloud_backup_source: create::CloudBackupSourceParams {
                    from_backup_id: None,
                    from_instance: None,
                },
                port: Some(port),
                start_conf: None,
                default_user: None,
                non_interactive: true,
                cloud_opts: cmd.cloud_opts.clone(),
                default_branch: database.name().map(|s| s.to_string()),
            },
            name,
            port,
        )?;
        create::create_service(&InstanceInfo {
            name: name.into(),
            instance_name: inst_name.clone(),
            installation: None,
            port,
            upgrade_state: None,
        })?;
        project::InstanceKind::Wsl
    } else {
        let inst = install::package(pkg).context(concatcp!("error installing ", BRANDING))?;
        let version = inst.version.specific();
        let info = InstanceInfo {
            name: name.into(),
            instance_name: inst_name.clone(),
            installation: Some(inst),
            port,
            upgrade_state: None,
        };
        create::bootstrap(
            &paths,
            &info,
            create::get_default_user_name(&version),
            database,
        )?;
        match create::create_service(&info) {
            Ok(()) => {}
            Err(e) => {
                log::warn!("Error running {BRANDING} as a service: {e:#}");
                print::warn!(
                    "{BRANDING} will not start on next login. \
                             Trying to start database in the background..."
                );
                control::start(&control::Start {
                    instance_opts: inst_name.into(),
                    foreground: false,
                    auto_restart: false,
                    managed_by: None,
                })?;
            }
        }
        project::InstanceKind::Portable(info)
    };

    let handle = project::Handle {
        name: name.into(),
        project_dir: project.location.root.clone(),
        schema_dir: project.resolve_schema_dir()?,
        instance,
        database: cmd.database().clone(),
    };

    let mut stash = project::StashDir::new(&project.location.root, name);
    stash.database = handle.database.as_deref();
    stash.write(stash_dir)?;

    if !opts.skip_hooks {
        hooks::on_action_sync("project.init.after", project)?;
    }

    if !cmd.no_migrations {
        migrate(&handle, false, opts.skip_hooks)?;
    } else {
        create_database(&handle)?;
    }

    project::config::apply_sync(project)?;

    print_initialized(name, &cmd.project_dir);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn do_cloud_init(
    name: CloudName,
    stash_dir: &Path,
    project: &project::Context,
    version: &ver::Specific,
    database: DatabaseBranch,
    cmd: &Command,
    opts: &crate::options::Options,
    client: &CloudClient,
) -> anyhow::Result<()> {
    let request = CloudInstanceCreate {
        name: name.name.clone(),
        version: version.to_string(),
        region: None,
        tier: None,
        requested_resources: None,
        source_instance_id: None,
        source_backup_id: None,
    };
    crate::cloud::ops::create_cloud_instance(client, &name.org_slug, request)?;
    let full_name = name.to_string();

    let handle = project::Handle {
        name: full_name.clone(),
        project_dir: project.location.root.clone(),
        schema_dir: project.resolve_schema_dir()?,
        instance: project::InstanceKind::Remote,
        database: database.name().map(|s| s.to_string()),
    };

    let mut stash = project::StashDir::new(&project.location.root, &full_name);
    stash.cloud_profile = client.profile.as_deref().or(Some("default"));
    stash.database = handle.database.as_deref();
    stash.write(stash_dir)?;

    if !opts.skip_hooks {
        hooks::on_action_sync("project.init.after", project)?;
    }

    if !cmd.no_migrations {
        migrate(&handle, false, opts.skip_hooks)?;
    } else {
        create_database(&handle)?;
    }
    print_initialized(&full_name, &cmd.project_dir);
    Ok(())
}

fn link(
    cmd: &Command,
    project: project::Location,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    msg!(
        "Found `{}` in {}",
        project
            .manifest
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        project.root.display()
    );
    msg!("Linking project...");

    let stash_dir = get_stash_path(&project.root)?;
    if stash_dir.exists() {
        anyhow::bail!("Project is already linked");
    }

    let project = project::load_ctx_at(project)?;
    let ver_query = &project.manifest.instance.server_version;

    let mut client = CloudClient::new(&opts.cloud_options)?;
    let name = if let Some(name) = &cmd.server_instance {
        name.clone()
    } else if !cmd.interactive {
        anyhow::bail!(
            "Existing instance name should be specified \
                       with `--server-instance` when linking project \
                       in non-interactive mode"
        )
    } else {
        ask_existing_instance_name(&mut client)?
    };
    let schema_dir = project.resolve_schema_dir()?;
    let mut inst = project::Handle::probe(&name, &project.location.root, &schema_dir, &client)?;
    if matches!(name, InstanceName::Cloud { .. }) {
        if !cmd.interactive {
            inst.database = Some(
                cmd.database()
                    .clone()
                    .unwrap_or(DEFAULT_DATABASE_NAME.to_string()),
            )
        } else {
            inst.database = ask_database()?.database().map(|s| s.to_string());
        }
    } else {
        inst.database.clone_from(cmd.database());
    }
    inst.check_version(ver_query);
    do_link(&inst, &project, cmd, &stash_dir, opts)
}

fn do_link(
    inst: &project::Handle,
    project: &project::Context,
    cmd: &Command,
    stash_dir: &Path,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    let mut stash = project::StashDir::new(&inst.project_dir, &inst.name);
    if let project::InstanceKind::Cloud { cloud_client, .. } = inst.instance {
        let profile = cloud_client.profile.as_deref().unwrap_or("default");
        stash.cloud_profile = Some(profile);
    };
    stash.database = inst.database.as_deref();
    stash.write(stash_dir)?;

    if !opts.skip_hooks {
        hooks::on_action_sync("project.init.after", project)?;
    }

    if !cmd.no_migrations {
        migrate(inst, cmd.interactive, opts.skip_hooks)?;
    } else {
        create_database(inst)?;
    }

    project::config::apply_sync(project)?;

    print::success!("Project linked");
    if let Some(dir) = &cmd.project_dir {
        eprintln!(
            "To connect to {}, navigate to {} and run `{BRANDING_CLI_CMD}`",
            inst.name,
            dir.display()
        );
    } else {
        eprintln!("To connect to {}, run `{BRANDING_CLI_CMD}`", inst.name);
    }

    Ok(())
}

fn directory_to_name(path: &Path, default: InstanceName) -> InstanceName {
    let path_stem = path.file_stem().and_then(|s| s.to_str());
    let Some(path_stem) = path_stem else {
        return default;
    };
    let stem = path_stem.replace(|c: char| !c.is_ascii_alphanumeric(), "_");
    let stem = stem.trim_matches('_');
    if stem.is_empty() {
        default
    } else {
        InstanceName::Local(stem.to_string())
    }
}

fn init_new(
    cmd: &Command,
    location: project::Location,
    opts: &crate::options::Options,
) -> anyhow::Result<()> {
    eprintln!(
        "No {MANIFEST_FILE_DISPLAY_NAME} found in `{}` or above",
        location.root.display()
    );

    let stash_dir = get_stash_path(&location.root)?;
    if stash_dir.exists() {
        anyhow::bail!(
            "{MANIFEST_FILE_DISPLAY_NAME} deleted after \
                       project initialization. \
                       Please run `{BRANDING_CLI_CMD} project unlink -D` to \
                       clean up old database instance."
        );
    }

    if !cmd.interactive {
        eprintln!("Initializing new project...");
    } else {
        let mut q = question::Confirm::new("Do you want to initialize a new project?");
        q.default(true);
        if !q.ask()? {
            return Err(ExitCode::new(0).into());
        }
    }

    let schema_dir = Path::new("dbschema");
    let schema_dir_path = location.root.join(schema_dir);
    let schema_files = project::find_schema_files(schema_dir)?;

    let mut client = CloudClient::new(&opts.cloud_options)?;
    let (inst_name, exists) = ask_name(&location.root, cmd, &mut client)?;

    if exists {
        let mut inst;
        inst = project::Handle::probe(&inst_name, &location.root, schema_dir, &client)?;
        let specific_version: &Specific = &inst.get_version()?.specific();
        let version_query = Query::from_version(specific_version)?;

        let manifest = project::manifest::Manifest {
            instance: project::manifest::Instance {
                server_version: version_query,
            },
            project: None,
            hooks: None,
            hooks_extend: None,
            watch: Vec::new(),
            generate: None,
        };
        project::manifest::write(&location.manifest, &manifest)?;
        let ctx = project::Context::new(location, manifest)?;
        if !schema_files {
            project::write_schema_default(&schema_dir_path, &ctx.manifest.instance.server_version)?;
        }
        if matches!(inst_name, InstanceName::Cloud { .. }) {
            if !cmd.interactive {
                inst.database = Some(
                    cmd.database()
                        .clone()
                        .unwrap_or(get_default_branch_or_database(specific_version)),
                );
            } else {
                inst.database = ask_database_or_branch(specific_version)?
                    .name()
                    .map(|s| s.to_string());
            }
        } else {
            inst.database.clone_from(cmd.database());
        }
        return do_link(&inst, &ctx, cmd, &stash_dir, opts);
    };

    match &inst_name {
        InstanceName::Cloud(name) => {
            msg!("Checking {BRANDING_CLOUD} versions...");
            client.ensure_authenticated()?;

            let (ver_query, version) = ask_cloud_version(cmd, &client)?;
            ver::print_version_hint(&version, &ver_query);
            let database = ask_database_or_branch(&version)?;
            table::settings(&[
                ("Project directory", location.root.display().to_string()),
                ("Project config", location.manifest.display().to_string()),
                (
                    &format!(
                        "Schema dir {}",
                        if schema_files {
                            "(non-empty)"
                        } else {
                            "(empty)"
                        }
                    ),
                    schema_dir_path.display().to_string(),
                ),
                (
                    if version.major >= 5 {
                        "Branch"
                    } else {
                        "Database"
                    },
                    database.to_string(),
                ),
                ("Version", version.to_string()),
                ("Instance name", name.to_string()),
            ]);

            let manifest = project::manifest::Manifest {
                instance: project::manifest::Instance {
                    server_version: ver_query,
                },
                project: Default::default(),
                hooks: None,
                hooks_extend: None,
                watch: Vec::new(),
                generate: None,
            };
            project::manifest::write(&location.manifest, &manifest)?;
            let ctx = project::Context::new(location, manifest)?;
            if !schema_files {
                project::write_schema_default(&schema_dir_path, &Query::from_version(&version)?)?;
            }

            do_cloud_init(
                name.to_owned(),
                &stash_dir,
                &ctx,
                &version,
                database,
                cmd,
                opts,
                &client,
            )
        }
        InstanceName::Local(name) => {
            msg!("Checking {BRANDING} versions...");
            let (ver_query, pkg) = ask_local_version(cmd)?;
            let specific_version = &pkg.version.specific();
            ver::print_version_hint(specific_version, &ver_query);

            let mut branch = DatabaseBranch::Default;
            if cmd.interactive && specific_version.major >= 5 {
                branch = ask_branch()?;
            }

            let meth = if cfg!(windows) {
                "WSL".to_string()
            } else {
                "portable package".to_string()
            };

            let schema_dir_key = &format!(
                "Schema dir {}",
                if schema_files {
                    "(non-empty)"
                } else {
                    "(empty)"
                }
            );

            let mut rows: Vec<(&str, String)> = vec![
                ("Project directory", location.root.display().to_string()),
                ("Project config", location.manifest.display().to_string()),
                (schema_dir_key, schema_dir_path.display().to_string()),
                ("Installation method", meth),
                ("Version", pkg.version.to_string()),
                ("Instance name", name.clone()),
            ];

            if let Some(branch) = branch.branch_for_create() {
                rows.push(("Branch", branch.to_string()))
            }

            table::settings(rows.as_slice());

            let manifest = project::manifest::Manifest {
                instance: project::manifest::Instance {
                    server_version: ver_query,
                },
                project: Default::default(),
                hooks: None,
                hooks_extend: None,
                watch: Vec::new(),
                generate: None,
            };

            project::manifest::write(&location.manifest, &manifest)?;
            let project = project::Context::new(location, manifest)?;
            if !schema_files {
                project::write_schema_default(
                    &schema_dir_path,
                    &Query::from_version(specific_version)?,
                )?;
            }

            do_init(name, &pkg, &stash_dir, &project, branch, cmd, opts)
        }
    }
}

fn ask_name(
    dir: &Path,
    options: &Command,
    cloud_client: &mut CloudClient,
) -> anyhow::Result<(InstanceName, bool)> {
    let instances = credentials::all_instance_names()?;
    let default_name = if let Some(name) = &options.server_instance {
        name.clone()
    } else {
        let base_name = directory_to_name(dir, InstanceName::Local("instance".to_string()));
        let mut name = base_name.clone();

        while credentials::exists(&name)? {
            name = InstanceName::Local(format!(
                "{}_{:04}",
                base_name,
                thread_rng().gen_range(0..10000)
            ));
        }
        name
    };
    if !options.interactive {
        let exists = match &default_name {
            InstanceName::Local(_) => instances.contains(&default_name),
            InstanceName::Cloud(name) => {
                cloud_client.ensure_authenticated()?;
                let inst = crate::cloud::ops::find_cloud_instance_by_name(name, cloud_client)?;
                inst.is_some()
            }
        };
        if exists {
            anyhow::bail!(format!(
                "Instance {:?} already exists, \
                               to link project with it pass `--link` \
                               flag explicitly",
                default_name.to_string()
            ))
        }
        return Ok((default_name, false));
    }
    loop {
        let default_name = default_name.to_string();
        let mut q = question::String::new(concatcp!(
            "Specify the name of the ",
            BRANDING,
            " instance to use with this project"
        ))
        .default(&default_name);
        let target_name = q.ask()?;
        let inst_name = match InstanceName::from_str(&target_name) {
            Ok(name) => name,
            Err(e) => {
                print::error!("{e}");
                continue;
            }
        };
        let exists = match &inst_name {
            InstanceName::Local(_) => instances.contains(&inst_name),
            InstanceName::Cloud(name) => {
                if !cloud_client.is_logged_in {
                    if let Err(e) = crate::cloud::ops::prompt_cloud_login(cloud_client) {
                        print::error!("{e}");
                        continue;
                    }
                }
                crate::cloud::ops::find_cloud_instance_by_name(name, cloud_client)?.is_some()
            }
        };
        if exists {
            let confirm = question::Confirm::new(format!(
                "Do you want to use existing instance {target_name:?} \
                         for the project?"
            ));
            if confirm.ask()? {
                return Ok((inst_name, true));
            }
        } else {
            return Ok((inst_name, false));
        }
    }
}

fn get_default_branch_or_database(version: &Specific) -> String {
    if version.major >= 5 {
        return String::from(DEFAULT_BRANCH_NAME_CREATE);
    }

    String::from(DEFAULT_DATABASE_NAME)
}

fn ask_database_or_branch(version: &Specific) -> anyhow::Result<DatabaseBranch> {
    if version.major >= 5 {
        return ask_branch();
    }

    ask_database()
}

fn ask_database() -> anyhow::Result<DatabaseBranch> {
    let mut q = question::String::new("Specify database name:").default(DEFAULT_DATABASE_NAME);
    loop {
        let name = q.ask()?;
        if name.trim().is_empty() {
            print::error!("Non-empty name is required");
        } else {
            return Ok(DatabaseBranch::Database(name.trim().to_string()));
        }
    }
}

fn ask_branch() -> anyhow::Result<DatabaseBranch> {
    let mut q = question::String::new("Specify branch name:").default(DEFAULT_BRANCH_NAME_CREATE);
    loop {
        let name = q.ask()?;
        if name.trim().is_empty() {
            print::error!("Non-empty name is required");
        } else {
            return Ok(DatabaseBranch::Branch(name.trim().to_string()));
        }
    }
}

fn ask_local_version(options: &Command) -> anyhow::Result<(Query, PackageInfo)> {
    let ver_query = options.server_version.clone().unwrap_or(Query::stable());
    if !options.interactive || options.server_version.is_some() {
        let pkg = repository::get_server_package(&ver_query)?
            .with_context(|| format!("no package matching {} found", ver_query.display()))?;
        if options.server_version.is_some() {
            return Ok((ver_query, pkg));
        } else {
            return Ok((Query::from_version(&pkg.version.specific())?, pkg));
        }
    }
    let default = repository::get_server_package(&ver_query)?;
    let default_ver = if let Some(pkg) = &default {
        Query::from_version(&pkg.version.specific())?.as_config_value()
    } else {
        String::new()
    };
    let mut q = question::String::new(concatcp!(
        "Specify the version of the ",
        BRANDING,
        " instance to use with this project"
    ))
    .default(&default_ver);
    loop {
        let value = q.ask()?;
        let value = value.trim();
        if value == "nightly" {
            match repository::get_server_package(&Query::nightly()) {
                Ok(Some(pkg)) => return Ok((Query::nightly(), pkg)),
                Ok(None) => {
                    print::error!("No nightly versions found");
                    continue;
                }
                Err(e) => {
                    print::error!("Cannot find nightly version: {e}");
                    continue;
                }
            }
        } else if value == "testing" {
            match repository::get_server_package(&Query::testing()) {
                Ok(Some(pkg)) => return Ok((Query::testing(), pkg)),
                Ok(None) => {
                    print::error!("No testing versions found");
                    continue;
                }
                Err(e) => {
                    print::error!("Cannot find testing version: {e}");
                    continue;
                }
            }
        } else {
            match parse_ver_and_find(value) {
                Ok(Some(pair)) => return Ok(pair),
                Ok(None) => {
                    print::error!("No matching packages found");
                    print_versions("Available versions")?;
                    continue;
                }
                Err(e) => {
                    print::error!("{e}");
                    print_versions("Available versions")?;
                    continue;
                }
            }
        }
    }
}

fn ask_existing_instance_name(cloud_client: &mut CloudClient) -> anyhow::Result<InstanceName> {
    let instances = credentials::all_instance_names()?;

    loop {
        let mut q = question::String::new(concatcp!(
            "Specify the name of the ",
            BRANDING,
            " instance to link with this project"
        ));
        let target_name = q.ask()?;

        let inst_name = match InstanceName::from_str(&target_name) {
            Ok(name) => name,
            Err(e) => {
                print::error!("{e}");
                continue;
            }
        };
        let exists = match &inst_name {
            InstanceName::Local(_) => instances.contains(&inst_name),
            InstanceName::Cloud(name) => {
                if !cloud_client.is_logged_in {
                    if let Err(e) = crate::cloud::ops::prompt_cloud_login(cloud_client) {
                        print::error!("{e}");
                        continue;
                    }
                }
                crate::cloud::ops::find_cloud_instance_by_name(name, cloud_client)?.is_some()
            }
        };
        if exists {
            return Ok(inst_name);
        } else {
            print::error!("Instance {target_name:?} does not exist");
        }
    }
}

fn parse_ver_and_find(value: &str) -> anyhow::Result<Option<(Query, PackageInfo)>> {
    let filter = value.parse()?;
    let query = Query::from_filter(&filter)?;
    if let Some(pkg) = repository::get_server_package(&query)? {
        Ok(Some((query, pkg)))
    } else {
        Ok(None)
    }
}

fn print_versions(title: &str) -> anyhow::Result<()> {
    let mut avail = repository::get_server_packages(Channel::Stable)?;
    avail.sort_by(|a, b| b.version.cmp(&a.version));
    println!(
        "{}: {}{}",
        title,
        avail
            .iter()
            .filter_map(|p| Query::from_version(&p.version.specific()).ok())
            .take(5)
            .map(|v| v.as_config_value())
            .collect::<Vec<_>>()
            .join(", "),
        if avail.len() > 5 { " ..." } else { "" },
    );
    Ok(())
}

fn parse_ver_and_find_cloud(
    value: &str,
    client: &CloudClient,
) -> anyhow::Result<(Query, ver::Specific)> {
    let filter = value.parse()?;
    let query = Query::from_filter(&filter)?;
    let version = cloud::versions::get_version(&query, client)?;
    Ok((query, version))
}

fn ask_cloud_version(
    options: &Command,
    client: &CloudClient,
) -> anyhow::Result<(Query, ver::Specific)> {
    let ver_query = options.server_version.clone().unwrap_or(Query::stable());
    if !options.interactive || options.server_version.is_some() {
        let version = cloud::versions::get_version(&ver_query, client)?;
        return Ok((ver_query, version));
    }
    let default = cloud::versions::get_version(&Query::stable(), client)?;
    let default_ver = Query::from_version(&default)?.as_config_value();
    let mut q = question::String::new(concatcp!(
        "Specify the version of the ",
        BRANDING,
        " instance to use with this project"
    ))
    .default(&default_ver);
    loop {
        let value = q.ask()?;
        let value = value.trim();
        if value == "nightly" {
            match cloud::versions::get_version(&Query::nightly(), client) {
                Ok(v) => return Ok((Query::nightly(), v)),
                Err(e) => {
                    print::error!("{e}");
                    continue;
                }
            }
        } else if value == "testing" {
            match cloud::versions::get_version(&Query::testing(), client) {
                Ok(v) => return Ok((Query::testing(), v)),
                Err(e) => {
                    print::error!("{e}");
                    continue;
                }
            }
        } else {
            match parse_ver_and_find_cloud(value, client) {
                Ok(pair) => return Ok(pair),
                Err(e) => {
                    print::error!("{e}");
                    print_cloud_versions("Available versions", client)?;
                    continue;
                }
            }
        }
    }
}

fn print_cloud_versions(title: &str, client: &CloudClient) -> anyhow::Result<()> {
    let mut avail: Vec<ver::Specific> = cloud::ops::get_versions(client)?
        .into_iter()
        .map(|v| v.version.parse::<ver::Specific>().unwrap())
        .collect();
    avail.sort();
    println!(
        "{}: {}{}",
        title,
        avail
            .iter()
            .filter_map(|p| Query::from_version(p).ok())
            .take(5)
            .map(|v| v.as_config_value())
            .collect::<Vec<_>>()
            .join(", "),
        if avail.len() > 5 { " ..." } else { "" },
    );
    Ok(())
}

fn print_initialized(name: &str, dir_option: &Option<PathBuf>) {
    print::success!("Project initialized.");
    if let Some(dir) = dir_option {
        msg!(
            "To connect to {}, navigate to {} and run `{}`",
            name.emphasized(),
            dir.display(),
            BRANDING_CLI_CMD
        );
    } else {
        msg!(
            "To connect to {}, run `{}`",
            name.emphasized(),
            BRANDING_CLI_CMD
        );
    }
}

#[tokio::main(flavor = "current_thread")]
async fn create_database(inst: &project::Handle<'_>) -> anyhow::Result<()> {
    Box::pin(create_database_async(inst)).await
}

async fn create_database_async(inst: &project::Handle<'_>) -> anyhow::Result<()> {
    let Some(name) = &inst.database else {
        return Ok(());
    };
    let config = inst.get_default_builder()?.build()?;
    let mut conn = Connection::connect(&config, QUERY_TAG).await?;
    ensure_database(&mut conn, name).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn migrate(
    inst: &project::Handle<'_>,
    ask_for_running: bool,
    skip_hooks: bool,
) -> anyhow::Result<()> {
    Box::pin(migrate_async(inst, ask_for_running, skip_hooks)).await
}

async fn migrate_async(
    inst: &project::Handle<'_>,
    ask_for_running: bool,
    skip_hooks: bool,
) -> anyhow::Result<()> {
    use crate::commands::Options;
    use crate::migrations::options::MigrationConfig;

    #[derive(Clone, Copy)]
    enum Action {
        Retry,
        Service,
        Run,
        Skip,
    }

    msg!("Applying migrations...");

    let mut conn = loop {
        match Box::pin(inst.get_default_connection()).await {
            Ok(conn) => break conn,
            Err(e) if ask_for_running && inst.instance.is_local() => {
                print::error!("{e}");
                let mut q = question::Numeric::new(format!(
                    "Cannot connect to instance {:?}. Options:",
                    inst.name,
                ));
                q.option("Start the service (if possible).", Action::Service);
                q.option(
                    "Start in the foreground, \
                          apply migrations and shut down.",
                    Action::Run,
                );
                q.option(
                    "Instance has been started manually, retry connect",
                    Action::Retry,
                );
                q.option("Skip migrations.", Action::Skip);
                match q.async_ask().await? {
                    Action::Service => match start(inst) {
                        Ok(()) => continue,
                        Err(e) => {
                            print::error!("{e}");
                            continue;
                        }
                    },
                    Action::Run => {
                        run_and_migrate(inst, skip_hooks)?;
                        return Ok(());
                    }
                    Action::Retry => continue,
                    Action::Skip => {
                        print::warn!("Skipping migrations.");
                        msg!(
                            "You can use `{BRANDING_CLI_CMD} migrate` to apply migrations \
                               once the service is up and running."
                        );
                        return Ok(());
                    }
                }
            }
            Err(e) => return Err(e)?,
        };
    };
    if let Some(database) = &inst.database {
        ensure_database(&mut conn, database).await?;
        conn = Box::pin(inst.get_connection()).await?;
    }

    migrations::apply::run(
        &migrations::apply::Command {
            cfg: MigrationConfig {
                schema_dir: Some(inst.project_dir.join(&inst.schema_dir)),
            },
            quiet: false,
            to_revision: None,
            dev_mode: false,
            single_transaction: false,
            no_index_build: false,
            conn: None,
        },
        &mut conn,
        &Options {
            command_line: true,
            styler: None,
            conn_params: Connector::new(inst.get_builder()?.build().map_err(Into::into)),
            instance_name: Some(InstanceName::Local(inst.name.clone())),
            skip_hooks,
        },
        true,
    )
    .await?;
    Ok(())
}

fn run_and_migrate(info: &project::Handle, skip_hooks: bool) -> anyhow::Result<()> {
    match &info.instance {
        project::InstanceKind::Portable(inst) => {
            control::ensure_runstate_dir(&info.name)?;
            let mut cmd = control::get_server_cmd(inst, false)?;
            cmd.background_for(|| Ok(migrate_async(info, false, skip_hooks)))?;
            Ok(())
        }
        project::InstanceKind::Wsl => {
            let mut cmd = windows::server_cmd(&info.name, false)?;
            cmd.background_for(|| Ok(migrate_async(info, false, skip_hooks)))?;
            Ok(())
        }
        project::InstanceKind::Remote => {
            anyhow::bail!(
                "remote instance not running, \
                          cannot run migrations"
            );
        }
        project::InstanceKind::Cloud { .. } => todo!(),
    }
}

async fn ensure_database(cli: &mut Connection, name: &str) -> anyhow::Result<()> {
    let name = quote_name(name);
    match cli.execute(&format!("CREATE DATABASE {name}"), &()).await {
        Ok(_) => Ok(()),
        Err(e) if e.is::<DuplicateDatabaseDefinitionError>() => Ok(()),
        Err(e) => Err(e)?,
    }
}

fn start(handle: &project::Handle) -> anyhow::Result<()> {
    match &handle.instance {
        project::InstanceKind::Portable(inst) => {
            control::do_start(inst)?;
            Ok(())
        }
        project::InstanceKind::Wsl => {
            windows::daemon_start(&handle.name)?;
            Ok(())
        }
        project::InstanceKind::Remote => {
            anyhow::bail!(
                "remote instance not running, \
                          cannot run migrations"
            );
        }
        project::InstanceKind::Cloud { .. } => todo!(),
    }
}
