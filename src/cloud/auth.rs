use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use fs_err as fs;
use tokio::time::sleep;

use crate::branding::BRANDING_CLOUD;
use crate::browser::open_link;
use crate::cloud::client::{CloudClient, CloudConfig, cloud_config_dir, cloud_config_file};
use crate::cloud::options;
use crate::commands::ExitCode;
use crate::options::CloudOptions;
use crate::portable::exit_codes;
use crate::portable::local::write_json;
use crate::print;
use crate::project::{find_project_stash_dirs, read_project_path};
use crate::question;
use gel_cli_instance::cloud::{
    CloudError, CreateSecretKeyInput, SecretKey, User, UserSession, UserSessionCreated,
};

const AUTHENTICATION_WAIT_TIME: Duration = Duration::from_secs(10 * 60);
const AUTHENTICATION_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub fn login(_c: &options::Login, options: &CloudOptions) -> anyhow::Result<()> {
    let mut client = CloudClient::new(options)?;
    do_login(&mut client)
}

#[tokio::main(flavor = "current_thread")]
pub async fn do_login(client: &mut CloudClient) -> anyhow::Result<()> {
    _do_login(client).await
}

pub async fn _do_login(client: &mut CloudClient) -> anyhow::Result<()> {
    // See if we're already logged in.
    match client.api.get_user().await {
        Ok(user) => {
            print::success!("Already logged in as {}.", user.name);
            return Ok(());
        }
        Err(CloudError::Unauthorized) => {
            // Fallthrough.
        }
        Err(err) => {
            return Err(err.into());
        }
    }

    let UserSessionCreated { id, auth_url } = client.api.create_session("CLI").await?;
    {
        let link = client.api_endpoint.join(&auth_url)?.to_string();
        let success_prompt = "Complete the authentication process now open in your browser";
        let error_prompt = "Please paste this link into your browser to complete authentication:";
        open_link(&link, Some(success_prompt), Some(error_prompt));
    }
    let deadline = Instant::now() + AUTHENTICATION_WAIT_TIME;
    while Instant::now() < deadline {
        match client.api.get_session(&id).await {
            Ok(UserSession {
                id: _,
                auth_url: _,
                token: Some(secret_key),
            }) => {
                // `token` is a short-lived secret key, obtain a
                // non-expiring secret key from the secretkeys/ API now.
                client.set_secret_key(Some(&secret_key))?;
                let hostname = gethostname::gethostname();
                let key: SecretKey = client
                    .api
                    .create_secret_key(CreateSecretKeyInput {
                        name: Some(format!("CLI @ {hostname:#?}")),
                        description: None,
                        scopes: None,
                        ttl: None,
                    })
                    .await?;

                write_json(
                    &cloud_config_file(&client.profile)?,
                    "cloud config",
                    &CloudConfig {
                        secret_key: key.secret_key,
                    },
                )?;
                client.set_secret_key(None)?;

                let user: User = client.api.get_user().await?;
                print::success!(
                    "Successfully logged in to {BRANDING_CLOUD} as {}.",
                    user.name
                );
                return Ok(());
            }
            Err(e) => print::warn!("Request failed: {e:?}\nRetrying..."),
            _ => {}
        }
        sleep(AUTHENTICATION_POLL_INTERVAL).await;
    }
    anyhow::bail!(
        "Authentication expected to complete in {:?}.",
        AUTHENTICATION_WAIT_TIME
    )
}

fn find_project_dirs(f: impl Fn(&str) -> bool) -> anyhow::Result<HashMap<String, Vec<PathBuf>>> {
    let projects = find_project_stash_dirs("cloud-profile", f, false)?;
    Ok(projects
        .into_iter()
        .filter_map(|(profile, projects)| {
            let projects = projects
                .into_iter()
                .filter_map(|p| {
                    read_project_path(&p)
                        .inspect_err(|_| {
                            log::warn!("Broken project stash dir: {p:?}");
                        })
                        .ok()
                })
                .collect::<Vec<_>>();
            if projects.is_empty() {
                None
            } else {
                Some((profile, projects))
            }
        })
        .collect())
}

pub fn logout(c: &options::Logout, options: &CloudOptions) -> anyhow::Result<()> {
    let mut warnings = Vec::new();
    let mut skipped = false;
    let mut removed = false;
    if c.all_profiles {
        let cloud_creds = cloud_config_dir()?;
        let dir_entries = match fs::read_dir(cloud_creds.clone()) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => anyhow::bail!(e),
        };
        let mut projects = find_project_dirs(|_| true)
            .or_else(|e| if c.force { Ok(HashMap::new()) } else { Err(e) })?;
        for item in dir_entries {
            let item = item?;
            let sub_dir = item.path();
            let stem = sub_dir.file_stem().and_then(|s| s.to_str());
            if stem.map(|n| n.starts_with('.')).unwrap_or(true) {
                // skip hidden files, most likely .DS_Store
                continue;
            }
            let profile = stem.unwrap();
            log::debug!("Logging out from profile {profile:?}");
            if let Some(projects) = projects.remove(profile) {
                if !projects.is_empty() {
                    if c.non_interactive {
                        warnings.push((profile.to_string(), projects));
                        if !c.force {
                            skipped = true;
                            continue;
                        }
                    } else {
                        let q = question::Confirm::new_dangerous(format!(
                            "{}\nStill log out?",
                            make_project_warning(profile, projects),
                        ));
                        if !q.ask()? {
                            skipped = true;
                            continue;
                        }
                    }
                }
            }
            removed = true;
            fs::remove_file(cloud_creds.join(item.file_name()))?;
            print::success!("You are now logged out from {BRANDING_CLOUD} profile {profile:?}.");
        }
    } else {
        let client = CloudClient::new(options)?;
        let path = cloud_config_file(&client.profile)?;
        if path.exists() {
            let profile = client.profile.as_deref().unwrap_or("default");
            log::debug!("Logging out from profile {profile:?}");
            let projects = find_project_dirs(|p| profile == p)
                .map(|projects| projects.into_values().flatten().collect())
                .or_else(|e| if c.force { Ok(Vec::new()) } else { Err(e) })?;
            removed = true;
            if !projects.is_empty() {
                if c.non_interactive {
                    warnings.push((profile.to_string(), projects));
                    removed = c.force;
                } else {
                    let q = question::Confirm::new_dangerous(format!(
                        "{}\nStill log out?",
                        make_project_warning(profile, projects),
                    ));
                    removed = q.ask()?;
                }
            }
            if removed {
                fs::remove_file(path).with_context(|| "failed to log out")?;
                print::success!(
                    "You are now logged out from {BRANDING_CLOUD} for profile \"{}\".",
                    client.profile.as_deref().unwrap_or("default")
                );
            }
            skipped = !removed;
        } else {
            print::warn!(
                "Already logged out from {BRANDING_CLOUD} for profile \"{}\".",
                client.profile.as_deref().unwrap_or("default")
            );
        }
    }
    if !warnings.is_empty() {
        let message = warnings
            .into_iter()
            .map(|(profile, projects)| make_project_warning(&profile, projects))
            .collect::<Vec<_>>()
            .join("\n");
        if c.force {
            print::warn!("{message}");
        } else {
            print::error!("{message}");
            Err(ExitCode::new(exit_codes::NEEDS_FORCE))?;
        }
    }
    if !skipped {
        Ok(())
    } else if removed {
        Err(ExitCode::new(exit_codes::PARTIAL_SUCCESS))?
    } else {
        Err(ExitCode::new(exit_codes::NEEDS_FORCE))?
    }
}

fn make_project_warning(profile: &str, projects: Vec<PathBuf>) -> String {
    format!(
        "{BRANDING_CLOUD} profile {:?} is still used by the following projects:\n    {}",
        profile,
        projects
            .iter()
            .filter_map(|p| p.to_str())
            .collect::<Vec<_>>()
            .join("\n    "),
    )
}
