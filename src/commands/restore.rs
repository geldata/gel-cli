use std::collections::BTreeSet;
use std::convert::TryInto;
use std::ffi::OsString;
use std::path::Path;
use std::pin::Pin;
use std::str;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use async_fn_stream::TryStreamEmitter;
use bytes::{Bytes, BytesMut};
use fn_error_context::context;
use futures_util::Stream;
use futures_util::stream::StreamExt;
use indicatif::{HumanBytes, ProgressBar};
use tokio::fs;
use tokio::io::{self, AsyncRead, AsyncReadExt};

use edgeql_parser::helpers::quote_name;
use edgeql_parser::preparser::is_empty;
use gel_errors::Error;

use crate::branding::BRANDING;
use crate::commands::Options;
use crate::commands::list_databases;
use crate::commands::parser::Restore as RestoreCmd;
use crate::connect::Connection;
use crate::locking::LockManager;
use crate::statement::{EndOfFile, read_statement};

type Input = Box<dyn AsyncRead + Unpin + Send>;

const MAX_SUPPORTED_DUMP_VER: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PacketType {
    Header,
    Block,
}

pub struct Packets {
    input: Pin<Box<dyn Stream<Item = Result<Bytes, Error>> + Send>>,
}

async fn packet_generator(
    emitter: TryStreamEmitter<Bytes, Error>,
    mut input: impl AsyncRead + Unpin + Send + 'static,
) -> Result<(), Error> {
    const HEADER_LEN: usize = 1 + 20 + 4;
    let mut buf = BytesMut::with_capacity(65536);
    let mut packet_index = 0;

    'outer: loop {
        while buf.len() < HEADER_LEN {
            buf.reserve(HEADER_LEN);
            let n = input
                .read_buf(&mut buf)
                .await
                .context("Cannot read packet header")?;
            if n == 0 {
                // EOF
                if buf.is_empty() {
                    break 'outer;
                } else {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof))
                        .context("Cannot read packet header")?;
                }
            }
        }

        let expected = if packet_index == 0 {
            PacketType::Header
        } else {
            PacketType::Block
        };

        let packet_type = match buf[0] {
            b'H' => PacketType::Header,
            b'D' => PacketType::Block,
            _ => {
                return Err(io::Error::from(io::ErrorKind::InvalidData))
                    .context(format!("Invalid block type {:x}", buf[0]))?;
            }
        };

        if packet_type != expected {
            return Err(io::Error::from(io::ErrorKind::InvalidData)).context(format!(
                "Expected type {expected:?}, got {packet_type:?} at packet {packet_index}"
            ))?;
        }

        let len = u32::from_be_bytes(buf[1 + 20..][..4].try_into().unwrap()) as usize;

        if buf.capacity() < HEADER_LEN + len {
            buf.reserve((HEADER_LEN + len - buf.capacity()).next_power_of_two());
        }

        while buf.len() < HEADER_LEN + len {
            let read = input
                .read_buf(&mut buf)
                .await
                .with_context(|| format!("Error reading block of {len} bytes"))?;
            if read == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof))
                    .with_context(|| format!("Error reading block of {len} bytes"))?;
            }
        }

        let block = buf.split_to(HEADER_LEN + len).split_off(HEADER_LEN);
        emitter.emit(block.freeze()).await;

        _ = buf.try_reclaim(len);
        packet_index += 1;
    }

    Ok(())
}

impl Packets {
    fn new(input: impl AsyncRead + Unpin + Send + 'static) -> Self {
        Packets {
            input: Box::pin(async_fn_stream::try_fn_stream(move |emitter| {
                packet_generator(emitter, input)
            })),
        }
    }
}

impl Stream for Packets {
    type Item = Result<Bytes, Error>;
    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, Error>>> {
        self.input.poll_next_unpin(cx)
    }
}

struct StreamWithProgress<T: Stream<Item = Result<Bytes, Error>> + Unpin> {
    input: T,
    bar: ProgressBar,
    progress: u64,
    total: Option<u64>,
    speed_checkpoint: (Instant, u64),
    last_estimated_speed: f64,
}

impl<T: Stream<Item = Result<Bytes, Error>> + Unpin> StreamWithProgress<T> {
    fn new(input: T, bar: ProgressBar, total: Option<u64>) -> Self {
        Self {
            input,
            bar,
            progress: 0,
            total,
            speed_checkpoint: (Instant::now(), 0),
            last_estimated_speed: 0.0,
        }
    }
}

impl<T: Stream<Item = Result<Bytes, Error>> + Unpin> Stream for StreamWithProgress<T> {
    type Item = Result<Bytes, Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let next = ready!(this.input.poll_next_unpin(cx));
        if let Some(Ok(block)) = &next {
            this.bar.tick();
            this.progress += block.len() as u64;

            let elapsed = this.speed_checkpoint.0.elapsed().as_secs_f64();
            let estimated_speed = if elapsed > 1.0 {
                let estimated_speed = (this.progress - this.speed_checkpoint.1) as f64 / elapsed;
                if this.speed_checkpoint.0.elapsed() > Duration::from_secs(30) {
                    this.speed_checkpoint = (Instant::now(), this.progress);
                }
                (estimated_speed + this.last_estimated_speed) / 2.0
            } else {
                this.last_estimated_speed
            };

            this.last_estimated_speed = estimated_speed;

            if let Some(total) = this.total {
                this.bar.set_message(format!(
                    "Restoring database: {}/{} processed ({}/s)",
                    HumanBytes(this.progress),
                    HumanBytes(total),
                    HumanBytes(estimated_speed as u64)
                ));
            } else {
                this.bar.set_message(format!(
                    "Restoring database: {} processed ({}/s)",
                    HumanBytes(this.progress),
                    HumanBytes(estimated_speed as u64)
                ));
            }
        } else {
            this.bar.set_message("Processing data");
            this.bar.finish();
        }
        Poll::Ready(next)
    }
}

#[context("error checking if DB is empty")]
async fn is_non_empty_db(cli: &mut Connection) -> Result<bool, anyhow::Error> {
    let non_empty = cli
        .query_required_single::<bool, _>(
            r###"SELECT
            count(
                schema::Module
                FILTER NOT .builtin AND NOT .name = "default"
            ) + count(
                schema::Object
                FILTER .name LIKE "default::%"
            ) > 0
        "###,
            &(),
        )
        .await?;
    return Ok(non_empty);
}

pub async fn restore(
    cli: &mut Connection,
    options: &Options,
    params: &RestoreCmd,
) -> Result<(), anyhow::Error> {
    let _lock = if let Some(instance) = &options.instance_name {
        Some(LockManager::lock_instance_async(instance).await?)
    } else {
        None
    };
    if params.all {
        Box::pin(restore_all(cli, options, params)).await
    } else {
        Box::pin(restore_db(cli, options, params)).await
    }
}

async fn restore_db(
    cli: &mut Connection,
    _options: &Options,
    params: &RestoreCmd,
) -> Result<(), anyhow::Error> {
    let RestoreCmd {
        path: ref filename,
        all: _,
        verbose: _,
        conn: _,
    } = *params;
    if is_non_empty_db(cli).await? {
        return Err(anyhow::anyhow!(
            "\
            cannot restore: the database is not empty"
        ));
    }

    let file_ctx = &|| format!("Failed to read dump {}", filename.display());
    let (mut input, file_size) = if filename.to_str() == Some("-") {
        (Box::new(io::stdin()) as Input, None)
    } else {
        let file = fs::File::open(filename).await.with_context(file_ctx)?;
        let file_size = file.metadata().await?.len();
        eprintln!(
            "\nRestoring database from file `{}`. Total size: {:.02} MB",
            filename.display(),
            file_size as f64 / 1048576.0
        );
        (Box::new(file) as Input, Some(file_size))
    };
    let mut buf = [0u8; 17 + 8];
    input
        .read_exact(&mut buf)
        .await
        .context("Cannot read header")
        .with_context(file_ctx)?;
    if &buf[..17] != b"\xFF\xD8\x00\x00\xD8EDGEDB\x00DUMP\x00" {
        Err(anyhow::anyhow!(
            "Incorrect header; file is not a dump from {BRANDING}"
        ))
        .with_context(file_ctx)?
    }
    let version = i64::from_be_bytes(buf[17..].try_into().unwrap());
    if version == 0 || version > MAX_SUPPORTED_DUMP_VER {
        Err(anyhow::anyhow!("Unsupported dump version {}", version)).with_context(file_ctx)?
    }
    let mut packets = Packets::new(input);
    let header = packets
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("Dump is empty"))??;
    let bar = ProgressBar::new_spinner();
    bar.set_message("Restoring database");
    let input = StreamWithProgress::new(packets, bar, file_size);

    cli.restore(header, input).await?;

    eprintln!("Restore completed");

    Ok(())
}

fn path_to_database_name(path: &Path) -> anyhow::Result<String> {
    let encoded = path
        .file_stem()
        .and_then(|x| x.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid dump filename {:?}", path))?;
    let decoded = urlencoding::decode(encoded)
        .with_context(|| format!("failed to decode filename {path:?}"))?;
    Ok(decoded.to_string())
}

async fn apply_init(cli: &mut Connection, path: &Path) -> anyhow::Result<()> {
    let mut input = fs::File::open(path).await?;
    let mut inbuf = BytesMut::with_capacity(8192);
    log::debug!("Restoring init script");
    loop {
        let stmt = match read_statement(&mut inbuf, &mut input).await {
            Ok(chunk) => chunk,
            Err(e) if e.is::<EndOfFile>() => break,
            Err(e) => return Err(e),
        };
        let stmt = str::from_utf8(&stmt[..]).context("can't decode statement")?;
        if !is_empty(stmt) {
            log::trace!("Executing {:?}", stmt);
            cli.execute(stmt, &())
                .await
                .with_context(|| format!("failed statement {stmt:?}"))?;
        }
    }
    Ok(())
}

pub async fn restore_all(
    cli: &mut Connection,
    options: &Options,
    params: &RestoreCmd,
) -> anyhow::Result<()> {
    let dir = &params.path;
    let filename = dir.join("init.edgeql");
    apply_init(cli, filename.as_ref())
        .await
        .with_context(|| format!("error applying init file {filename:?}"))?;

    let mut conn_params = options.conn_params.clone();
    conn_params.wait_until_available(Duration::from_secs(300));
    let mut params = params.clone();
    let dbs = list_databases::get_databases(cli).await?;
    let existing: BTreeSet<_> = dbs.into_iter().collect();

    let dump_ext = OsString::from("dump");
    let mut dir_list = fs::read_dir(&dir).await?;
    while let Some(entry) = dir_list.next_entry().await? {
        let path = entry.path();
        if path.extension() != Some(&dump_ext) {
            continue;
        }
        let database = path_to_database_name(&path)?;
        log::debug!("Restoring database {:?}", database);
        if !existing.contains(&database) {
            let stmt = format!("CREATE DATABASE {}", quote_name(&database));
            cli.execute(&stmt, &())
                .await
                .with_context(|| format!("error creating database {database:?}"))?;
        }
        conn_params.branch(&database)?;
        let mut db_conn = Box::pin(conn_params.connect())
            .await
            .with_context(|| format!("cannot connect to database {database:?}"))?;
        params.path = path;
        restore_db(&mut db_conn, options, &params)
            .await
            .with_context(|| format!("restoring database {database:?}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn test_packets() {
        let mut fake_stream = Vec::new();
        // The header is 1 + 20 + 4 bytes where the last four bytes are a length and we ignore the rest
        for packet in 0..100 {
            let len: u32 = 16 + packet;
            let mut buf = BytesMut::with_capacity(1 + 20 + 4 + len as usize);
            buf.extend_from_slice(&[0; 1 + 20 + 4]);
            buf[0] = if packet == 0 { b'H' } else { b'D' };
            buf[21..25].copy_from_slice(&len.to_be_bytes());
            fake_stream.extend_from_slice(&buf.freeze());
            fake_stream.extend_from_slice(&vec![b'.'; len as usize]);
        }

        // Use a tokio task with a duplex to feed the fake stream in chunks of 11 bytes
        let (mut tx, rx) = tokio::io::duplex(100);
        let task = tokio::spawn(async move {
            for chunk in fake_stream.chunks(11) {
                tx.write_all(chunk).await.unwrap();
            }
        });

        let mut packets = Packets::new(Box::new(rx));
        let mut packet = 0;
        while let Some(data) = packets.next().await {
            let data = data.unwrap();
            let expected = Bytes::from(vec![b'.'; 16 + packet]);
            assert_eq!(data, expected);
            packet += 1;
        }

        assert_eq!(packet, 100);
        task.await.unwrap();
    }
}
