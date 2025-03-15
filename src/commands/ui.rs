use std::io::{stdout, Write};

use anyhow::Context;
use gel_tokio::{CloudName, InstanceName};

use crate::branding::{BRANDING, BRANDING_CLI_CMD};
use crate::browser::open_link;
use crate::cloud;
use crate::commands::ExitCode;
use crate::options::{Options, UI};
use crate::portable::local;
use crate::portable::repository::USER_AGENT;
use crate::print::{self, msg};

pub fn show_ui(cmd: &UI, opts: &Options) -> anyhow::Result<()> {
    let connector = opts.block_on_create_connector()?;
    let cfg = connector.get()?;

    let url = match cfg.instance_name() {
        Some(InstanceName::Cloud(cloud_name)) => get_cloud_ui_url(cmd, cloud_name, cfg, opts)?,
        _ => get_local_ui_url(cmd, cfg)?,
    };

    if cmd.print_url {
        stdout()
            .lock()
            .write_all((url + "\n").as_bytes())
            .expect("stdout write succeeds");
        Ok(())
    } else {
        let error_prompt =
            format!("Please paste the URL below into your browser to launch the {BRANDING} UI:");
        match open_link(&url, None, Some(&error_prompt)) {
            true => Ok(()),
            false => Err(ExitCode::new(1).into()),
        }
    }
}

fn get_cloud_ui_url(
    cmd: &UI,
    cloud_name: &CloudName,
    cfg: &gel_tokio::Config,
    opts: &Options,
) -> anyhow::Result<String> {
    let client = cloud::client::CloudClient::new(&opts.cloud_options)?;
    client.ensure_authenticated()?;
    let url = if client.is_default_partition {
        format!("https://cloud.edgedb.com/{cloud_name}")
    } else {
        let inst = cloud::ops::find_cloud_instance_by_name(
            &cloud_name.name,
            &cloud_name.org_slug,
            &client,
        )?
        .ok_or_else(|| anyhow::anyhow!("instance not found"))?;
        match inst.ui_url {
            Some(url) => url,
            None => get_local_ui_url(cmd, cfg)?,
        }
    };
    Ok(url)
}

fn get_local_ui_url(cmd: &UI, cfg: &gel_tokio::Config) -> anyhow::Result<String> {
    let secret_key = _get_local_ui_secret_key(cfg)?;
    let mut url = _get_local_ui_url(cmd, cfg)?;

    if let Some(secret_key) = secret_key {
        url = format!("{url}?authToken={secret_key}");
    }

    Ok(url)
}

fn _get_local_ui_url(cmd: &UI, cfg: &gel_tokio::Config) -> anyhow::Result<String> {
    let mut url = cfg
        .http_url(false)
        .map(|s| s + "/ui")
        .context("connected via unix socket")?;
    if cmd.no_server_check {
        // We'll always use HTTP if --no-server-check is specified, depending on
        // the server to redirect to HTTPS if necessary.
    } else {
        let mut use_https = false;
        if cfg.local_instance_name().is_none() {
            let https_url = cfg
                .http_url(true)
                .map(|u| u + "/ui")
                .context("connected via unix socket")?;
            match open_url(&https_url).map(|r| r.status()) {
                Ok(reqwest::StatusCode::OK) => {
                    url = https_url;
                    use_https = true;
                }
                Ok(status) => {
                    msg!("{https_url} returned status code {status}, retry HTTP.");
                }
                Err(e) => {
                    msg!("Failed to probe {https_url}: {e:#}, retry HTTP.");
                }
            }
        }
        if !use_https {
            match open_url(&url).map(|r| r.status()) {
                Ok(reqwest::StatusCode::OK) => {}
                Ok(reqwest::StatusCode::NOT_FOUND) => {
                    print::error!("Web UI not served correctly by specified {BRANDING} server.");
                    msg!(
                        "  Try running the \
                        server with `--admin-ui=enabled`."
                    );
                    return Err(ExitCode::new(2).into());
                }
                Ok(status) => {
                    log::info!("GET {} returned status code {}", url, status);
                    print::error!(
                        "Web UI not served correctly by specified {BRANDING} server. \
                        Try `{BRANDING_CLI_CMD} instance logs -I <instance_name>` to see details."
                    );
                    return Err(ExitCode::new(3).into());
                }
                Err(e) => {
                    print::error!("cannot connect to {url}: {e:#}");
                    return Err(ExitCode::new(4).into());
                }
            }
        }
    }

    Ok(url)
}

fn _get_local_ui_secret_key(cfg: &gel_tokio::Config) -> anyhow::Result<Option<String>> {
    let local_inst = cfg.local_instance_name();
    let local_info = local_inst
        .map(local::InstanceInfo::try_read)
        .transpose()?
        .flatten();

    if let Some(key) = cfg.secret_key() {
        Ok(Some(key.to_owned()))
    } else if let Some(instance) = local_info {
        let key = jwt::LocalJWT::new(instance.name)
            .generate()
            .map_err(|e| {
                log::warn!("Cannot generate authToken: {:#}", e);
            })
            .ok();
        Ok(key)
    } else if matches!(local_inst, Some("_localdev")) {
        let key = jwt::LocalJWT::new("_localdev")
            .generate()
            .map_err(|e| {
                log::warn!("Cannot generate authToken: {:#}", e);
            })
            .ok();
        Ok(key)
    } else {
        Ok(None)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn open_url(url: &str) -> Result<reqwest::Response, reqwest::Error> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .no_proxy()
        .build()?
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
}

mod jwt {

    use std::collections::HashMap;
    use std::iter::FromIterator;

    use fs_err as fs;
    use gel_jwt::{KeyRegistry, PrivateKey, SigningContext};

    use crate::platform::data_dir;
    use crate::portable::local::{instance_data_dir, NonLocalInstance};

    #[derive(Debug, thiserror::Error)]
    #[error("Cannot read JOSE key file(s)")]
    pub struct ReadKeyError(anyhow::Error);

    pub struct LocalJWT {
        instance_name: String,
        jws_key: Option<KeyRegistry<PrivateKey>>,
    }

    impl LocalJWT {
        pub fn new(instance_name: impl Into<String>) -> Self {
            let instance_name = instance_name.into();
            Self {
                instance_name,
                jws_key: None,
            }
        }

        fn read_keys(&mut self) -> anyhow::Result<()> {
            let mut key_set = gel_jwt::KeyRegistry::default();

            #[cfg(windows)]
            {
                let key_text = windows::read_jws_key(&self.instance_name)?;
                key_set.add_from_any(&key_text)?;
                self.jws_key = Some(key_set);
                return Ok(());
            }

            use crate::cli::env::Env;
            let data_dir = if self.instance_name == "_localdev" {
                match Env::server_dev_dir()? {
                    Some(path) => path,
                    None => data_dir()?.parent().unwrap().join("_localdev"),
                }
            } else {
                instance_data_dir(&self.instance_name)?
            };
            if !data_dir.exists() {
                anyhow::bail!(NonLocalInstance);
            }
            for keys in ["edbjwskeys.pem", "edbjwskeys.json"] {
                if data_dir.join(keys).try_exists()? {
                    let key_text = fs::read(data_dir.join(keys))?;
                    key_set.add_from_any(&String::from_utf8_lossy(&key_text))?;
                }
            }

            self.jws_key = Some(key_set);
            Ok(())
        }

        /// Generate a legacy-style token.
        pub fn generate(&mut self) -> anyhow::Result<String> {
            self.read_keys().map_err(ReadKeyError)?;

            let key = self
                .jws_key
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("jws_key not set"))?;
            let ctx = SigningContext::default();
            let token = key.sign(
                HashMap::from_iter([("edgedb.server.any_role".to_string(), true.into())]),
                &ctx,
            )?;

            return Ok(format!("edbt_{token}"));
        }
    }
}
