use std::path::{Path, PathBuf};

use anyhow::Context;
use indicatif::{HumanBytes, ProgressBar};
use sha1::Digest;
use tokio::fs::{self, OpenOptions};
use tokio::io::{self, AsyncWrite, AsyncWriteExt};
use tokio::task;

use tokio_stream::StreamExt;

use gel_errors::{UnknownDatabaseError, UnsupportedFeatureError};

use crate::commands::Options;
use crate::commands::list_databases::get_databases;
use crate::commands::parser::{Dump as DumpOptions, DumpFormat};
use crate::connect::Connection;
use crate::hint::HintExt;
use crate::locking::LockManager;
use crate::platform::tmp_file_name;
use crate::portable::ver;

type Output = Box<dyn AsyncWrite + Unpin + Send>;

pub struct Guard {
    filenames: Option<(PathBuf, PathBuf, bool)>,
}

impl Guard {
    async fn open(filename: &Path, overwrite_existing: bool) -> anyhow::Result<(Output, Guard)> {
        if filename.to_str() == Some("-") {
            Ok((Box::new(io::stdout()), Guard { filenames: None }))
        } else if cfg!(windows) || filename.starts_with("/dev/") || filename.file_name().is_none() {
            let file = OpenOptions::new()
                .write(true)
                .create(overwrite_existing)
                .create_new(!overwrite_existing)
                .truncate(overwrite_existing)
                .open(&filename)
                .await
                .context(filename.display().to_string())?;
            Ok((Box::new(file), Guard { filenames: None }))
        } else {
            if !overwrite_existing && fs::metadata(&filename).await.is_ok() {
                anyhow::bail!(
                    "failed: target file already exists. Specify --overwrite-existing to replace."
                )
            }
            // Create .~.tmp file path, first remove if already existing
            let tmp_path = filename.with_file_name(tmp_file_name(filename));
            if fs::metadata(&tmp_path).await.is_ok() {
                fs::remove_file(&tmp_path).await.ok();
            }
            let tmp_file = fs::File::create(&tmp_path)
                .await
                .context(tmp_path.display().to_string())?;
            Ok((
                Box::new(tmp_file),
                Guard {
                    filenames: Some((tmp_path, filename.to_owned(), overwrite_existing)),
                },
            ))
        }
    }

    async fn commit(self) -> anyhow::Result<()> {
        if let Some((tmp_filename, filename, overwrite_existing)) = self.filenames {
            if overwrite_existing {
                fs::rename(tmp_filename, filename).await?;
            } else {
                task::spawn_blocking(move || {
                    // favor compatibility over atomicity
                    renamore::rename_exclusive_fallback(tmp_filename, filename)
                })
                .await
                // tokio::fs::asyncify() is private; do the same thing here
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "background task failed"))?
                .map_err(|e| anyhow::anyhow!(e).hint("specify --overwrite-existing to replace."))?;
            }
        }
        Ok(())
    }
}

pub async fn dump(
    cli: &mut Connection,
    general: &Options,
    options: &DumpOptions,
) -> Result<(), anyhow::Error> {
    let _lock = if let Some(instance) = &general.instance_name {
        Some(LockManager::lock_read_instance_async(&instance).await?)
    } else {
        None
    };

    if options.all {
        if let Some(dformat) = options.format {
            if dformat != DumpFormat::Dir {
                anyhow::bail!("only `--format=dir` is supported for `--all`");
            }
        } else {
            anyhow::bail!("`--format=dir` is required when using `--all`");
        }
        dump_all(cli, general, options.path.as_ref(), options.include_secrets).await
    } else {
        if options.format.is_some() {
            anyhow::bail!("`--format` is reserved for dump using `--all`");
        }
        dump_db(
            cli,
            general,
            options.path.as_ref(),
            options.include_secrets,
            options.overwrite_existing,
        )
        .await
    }
}

async fn dump_db(
    cli: &mut Connection,
    _options: &Options,
    filename: &Path,
    mut include_secrets: bool,
    overwrite_existing: bool,
) -> Result<(), anyhow::Error> {
    if cli.get_version().await?.specific() < "4.0-alpha.2".parse().unwrap() {
        include_secrets = false;
    }

    let dbname = cli.database().to_string();
    eprintln!("Starting dump for database `{dbname}`...");

    let (mut output, guard) = Guard::open(filename, overwrite_existing).await?;
    output
        .write_all(
            b"\xFF\xD8\x00\x00\xD8EDGEDB\x00DUMP\x00\
          \x00\x00\x00\x00\x00\x00\x00\x01",
        )
        .await?;

    let (header, mut blocks) = cli.dump(include_secrets).await?;

    // this is ensured because length in the protocol is u32 too
    assert!(header.data.len() <= u32::MAX as usize);

    let mut header_buf = Vec::with_capacity(25);

    header_buf.push(b'H');
    header_buf.extend(&sha1::Sha1::new_with_prefix(&header.data).finalize()[..]);
    header_buf.extend(&(header.data.len() as u32).to_be_bytes()[..]);
    output.write_all(&header_buf).await?;
    output.write_all(&header.data).await?;

    let bar = ProgressBar::new_spinner();
    let mut processed = 0;

    while let Some(packet) = blocks.next().await.transpose()? {
        let packet_length = packet.data.len();
        bar.tick();
        processed += packet_length;
        bar.set_message(format!(
            "Database `{dbname}` dump: {} processed.",
            HumanBytes(processed as u64)
        ));
        bar.message();

        // this is ensured because length in the protocol is u32 too
        assert!(packet_length <= u32::MAX as usize);

        header_buf.truncate(0);
        header_buf.push(b'D');
        header_buf.extend(&sha1::Sha1::new_with_prefix(&packet.data).finalize()[..]);
        header_buf.extend(&(packet_length as u32).to_be_bytes()[..]);
        output.write_all(&header_buf).await?;
        output.write_all(&packet.data).await?;
    }
    guard.commit().await?;
    bar.abandon_with_message(format!(
        "Finished dump for `{dbname}`. Total size: {}",
        HumanBytes(processed as u64)
    ));
    Ok(())
}

fn _has_config_dump_bug(v: ver::Specific) -> bool {
    // Does the version potentially have the config dump problem where
    // DESCRIBE INSTANCE CONFIG fails if the auth extension is loaded?
    // It is fixed by #8379.
    // (This probably gives a false negative on one 7.0 nightly, but
    // oh well.)
    if v.major != 6 {
        return false;
    }
    match v.minor {
        ver::MinorVersion::Minor(m) => m == 0,
        _ => true,
    }
}

pub async fn dump_all(
    cli: &mut Connection,
    options: &Options,
    dir: &Path,
    include_secrets: bool,
) -> Result<(), anyhow::Error> {
    let databases = get_databases(cli).await?;
    let version = cli.get_version().await?.specific();
    let config_res = cli
        .query_required_single("DESCRIBE SYSTEM CONFIG", &())
        .await;
    // There was a bug in many 6.x nightlies and in 6.0 final that
    // broke dump --all (and thus upgrade), because the config
    // couldn't be dumped. Detect it and do the best we can.
    let config = match config_res {
        Err(e) if _has_config_dump_bug(version) && e.is::<UnsupportedFeatureError>() => {
            eprintln!(
                "Warning: Dumping the system configuration hit a known bug in 6.0 nightlies."
            );
            eprintln!("Warning: The dump will omit system configuration.");
            "# SYSTEM CONFIGURATION OMITTED DUE TO DUMP BUG".to_string()
        }
        r => r?,
    };
    let roles: String = cli.query_required_single("DESCRIBE ROLES", &()).await?;

    fs::create_dir_all(dir).await?;

    let (mut init, guard) = Guard::open(&dir.join("init.edgeql"), true).await?;
    if !config.trim().is_empty() {
        init.write_all(b"# DESCRIBE SYSTEM CONFIG\n").await?;
        init.write_all(config.as_bytes()).await?;
        init.write_all(b"\n").await?;
    }
    if !roles.trim().is_empty() {
        init.write_all(b"# DESCRIBE ROLES\n").await?;
        init.write_all(roles.as_bytes()).await?;
        init.write_all(b"\n").await?;
    }
    guard.commit().await?;

    let mut conn_params = options.conn_params.clone();
    for database in &databases {
        match conn_params.branch(database)?.connect().await {
            Ok(mut db_conn) => {
                let filename = dir.join(&(urlencoding::encode(database) + ".dump")[..]);
                dump_db(&mut db_conn, options, &filename, include_secrets, true).await?;
            }
            Err(err) => {
                if let Some(e) = err.downcast_ref::<gel_errors::Error>() {
                    if e.is::<UnknownDatabaseError>() {
                        eprintln!("Database {database} no longer exists, skipping...");
                        continue;
                    }
                }
                return Err(err);
            }
        }
    }

    Ok(())
}
