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
        let ver = instance.get_version()?.specific();
        let legacy = ver < "3.0-alpha.1".parse().unwrap();
        let key = jwt::LocalJWT::new(instance.name, legacy)
            .generate()
            .map_err(|e| {
                log::warn!("Cannot generate authToken: {:#}", e);
            })
            .ok();
        Ok(key)
    } else if matches!(local_inst, Some("_localdev")) {
        let key = jwt::LocalJWT::new("_localdev", false)
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

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    use fs_err as fs;
    use ring::rand::SecureRandom;
    use ring::signature::KeyPair;
    use ring::{aead, agreement, digest, rand, signature};

    use crate::platform::data_dir;
    use crate::portable::local::{instance_data_dir, NonLocalInstance};

    #[derive(Debug, thiserror::Error)]
    #[error("Cannot read JOSE key file(s)")]
    pub struct ReadKeyError(anyhow::Error);

    pub struct LocalJWT {
        instance_name: String,
        legacy: bool,
        rng: rand::SystemRandom,
        jws_key: Option<Vec<u8>>,
        jwe_key: Option<Vec<u8>>,
    }

    impl LocalJWT {
        pub fn new(instance_name: impl Into<String>, legacy: bool) -> Self {
            let instance_name = instance_name.into();
            let rng = rand::SystemRandom::new();
            Self {
                instance_name,
                legacy,
                rng,
                jws_key: None,
                jwe_key: None,
            }
        }

        #[cfg(windows)]
        fn read_keys(&mut self) -> anyhow::Result<()> {
            use crate::portable::windows;
            if self.legacy {
                let (jws_key, jwe_key) = windows::read_jose_keys_legacy(&self.instance_name)?;
                self.jws_key = Some(jws_key);
                self.jwe_key = Some(jwe_key);
            } else {
                self.jws_key = Some(windows::read_jws_key(&self.instance_name)?);
            }
            Ok(())
        }
        #[cfg(not(windows))]
        fn read_keys(&mut self) -> anyhow::Result<()> {
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
            self.jws_key = Some(fs::read(data_dir.join("edbjwskeys.pem"))?);
            if self.legacy {
                self.jwe_key = Some(fs::read(data_dir.join("edbjwekeys.pem"))?);
            }
            Ok(())
        }

        pub fn generate(&mut self) -> anyhow::Result<String> {
            self.read_keys().map_err(ReadKeyError)?;

            let token = self.generate_token()?;
            if !self.legacy {
                return Ok(format!("edbt_{token}"));
            }

            self.generate_legacy_token(token)
        }

        fn generate_token(&mut self) -> anyhow::Result<String> {
            let jws_pem = pem::parse(self.jws_key.as_deref().expect("jws_key not set"))?;
            let rand = ring::rand::SystemRandom::new();

            let jws = signature::EcdsaKeyPair::from_pkcs8(
                &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                jws_pem.contents(),
                &rand,
            )?;
            let message = format!(
                "{}.{}",
                URL_SAFE_NO_PAD.encode(b"{\"typ\":\"JWT\",\"alg\":\"ES256\"}"),
                URL_SAFE_NO_PAD.encode(b"{\"edgedb.server.any_role\":true}"),
            );
            let signature = jws.sign(&self.rng, message.as_bytes())?;
            Ok(format!("{}.{}", message, URL_SAFE_NO_PAD.encode(signature),))
        }

        fn generate_legacy_token(&self, signed_token: String) -> anyhow::Result<String> {
            // Replace this ES256/ECDH-ES implementation using raw ring
            // with biscuit when the algorithms are supported in biscuit
            let jwe_pem = pem::parse(self.jwe_key.as_deref().expect("jwe_key not set"))?;
            let rand = ring::rand::SystemRandom::new();

            let jwe = signature::EcdsaKeyPair::from_pkcs8(
                &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
                jwe_pem.contents(),
                &rand,
            )?;

            let priv_key =
                agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &self.rng)?;
            let pub_key =
                agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, jwe.public_key().as_ref());
            let epk = priv_key.compute_public_key()?.as_ref().to_vec();
            let cek = agreement::agree_ephemeral(priv_key, &pub_key, |key_material| {
                let mut ctx = digest::Context::new(&digest::SHA256);
                ctx.update(&[0, 0, 0, 1]);
                ctx.update(key_material);
                ctx.update(&[0, 0, 0, 7]); // AlgorithmID
                ctx.update(b"A256GCM");
                ctx.update(&[0, 0, 0, 0]); // PartyUInfo
                ctx.update(&[0, 0, 0, 0]); // PartyVInfo
                ctx.update(&[0, 0, 1, 0]); // SuppPubInfo (bitsize=256)
                ctx.finish()
            })
            .map_err(|_| anyhow::anyhow!("Error occurred while deriving key for JWT"))?;
            let enc_key =
                aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, cek.as_ref())?);
            let x = URL_SAFE_NO_PAD.encode(&epk[1..33]);
            let y = URL_SAFE_NO_PAD.encode(&epk[33..]);
            let protected = format!(
                "{{\
                    \"alg\":\"ECDH-ES\",\"enc\":\"A256GCM\",\"epk\":{{\
                        \"crv\":\"P-256\",\"kty\":\"EC\",\"x\":\"{x}\",\"y\":\"{y}\"\
                    }}\
                }}"
            );
            let protected = URL_SAFE_NO_PAD.encode(protected.as_bytes());
            let mut nonce = vec![0; 96 / 8];
            self.rng.fill(&mut nonce)?;
            let mut in_out = signed_token.as_bytes().to_vec();
            let tag = enc_key.seal_in_place_separate_tag(
                aead::Nonce::try_assume_unique_for_key(&nonce)?,
                aead::Aad::from(protected.clone()),
                &mut in_out,
            )?;

            Ok(format!(
                "{}..{}.{}.{}",
                protected,
                URL_SAFE_NO_PAD.encode(nonce),
                URL_SAFE_NO_PAD.encode(in_out),
                URL_SAFE_NO_PAD.encode(tag.as_ref()),
            ))
        }
    }
}
