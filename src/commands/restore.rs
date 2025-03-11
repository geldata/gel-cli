use std::collections::BTreeSet;
use std::convert::TryInto;
use std::ffi::OsString;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::str;
use std::task::{ready, Context, Poll};
use std::time::Duration;

use anyhow::Context as _;
use bytes::{Bytes, BytesMut};
use fn_error_context::context;
use futures_util::stream::StreamExt;
use futures_util::FutureExt;
use indicatif::{HumanBytes, ProgressBar};
use tokio::fs;
use tokio::io::{self, AsyncRead, AsyncReadExt};
use tokio_stream::Stream;

use edgeql_parser::helpers::quote_name;
use edgeql_parser::preparser::is_empty;
use gel_errors::{Error, ErrorKind, UserError};

use crate::branding::BRANDING;
use crate::commands::list_databases;
use crate::commands::parser::Restore as RestoreCmd;
use crate::commands::Options;
use crate::connect::Connection;
use crate::statement::{read_statement, EndOfFile};

type Input = Box<dyn AsyncRead + Unpin + Send>;

const MAX_SUPPORTED_DUMP_VER: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PacketType {
    Header,
    Block,
}

pub struct Packets {
    input: Option<Input>,
    buf: Option<BytesMut>,
    future: Option<
        Pin<Box<dyn Future<Output = (Input, BytesMut, Option<Result<Bytes, Error>>)> + Send>>,
    >,
}

async fn read_packet(
    input: &mut Input,
    buf: &mut BytesMut,
    expected: PacketType,
) -> Result<Option<Bytes>, anyhow::Error> {
    const HEADER_LEN: usize = 1 + 20 + 4;
    while buf.len() < HEADER_LEN {
        buf.reserve(HEADER_LEN);
        let n = input
            .read_buf(buf)
            .await
            .context("Cannot read packet header")?;
        if n == 0 {
            // EOF
            if buf.is_empty() {
                return Ok(None);
            } else {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof))
                    .context("Cannot read packet header")?;
            }
        }
    }
    let typ = match buf[0] {
        b'H' => PacketType::Header,
        b'D' => PacketType::Block,
        _ => return Err(anyhow::anyhow!("Invalid block type {:x}", buf[0])),
    };
    if typ != expected {
        return Err(anyhow::anyhow!(
            "Expected block {:?}, got {:?}",
            expected,
            typ
        ));
    }
    let len = u32::from_be_bytes(buf[1 + 20..][..4].try_into().unwrap()) as usize;
    if buf.capacity() < HEADER_LEN + len {
        buf.reserve(HEADER_LEN + len - buf.capacity());
    }
    while buf.len() < HEADER_LEN + len {
        let read = input
            .read_buf(buf)
            .await
            .with_context(|| format!("Error reading block of {len} bytes"))?;
        if read == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof))
                .with_context(|| format!("Error reading block of {len} bytes"))?;
        }
    }
    Ok(Some(
        buf.split_to(HEADER_LEN + len)
            .split_off(HEADER_LEN)
            .freeze(),
    ))
}

impl Packets {
    fn next_packet(
        &mut self,
    ) -> impl Future<Output = (Input, BytesMut, Option<Result<Bytes, Error>>)> + Send {
        let mut input = self.input.take().unwrap();
        let mut buf = self.buf.take().unwrap();
        async move {
            let res = read_packet(&mut input, &mut buf, PacketType::Block)
                .await
                .map_err(UserError::with_source_ref)
                .transpose();
            (input, buf, res)
        }
    }
}

impl Stream for Packets {
    type Item = Result<Bytes, Error>;
    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, Error>>> {
        let mut future = self
            .future
            .take()
            .unwrap_or_else(|| self.next_packet().boxed());
        match future.as_mut().poll(cx) {
            Poll::Ready((input, buf, res)) => {
                self.future = None;
                self.input = Some(input);
                self.buf = Some(buf);
                Poll::Ready(res)
            }
            Poll::Pending => {
                self.future = Some(future);
                Poll::Pending
            }
        }
    }
}

struct StreamWithProgress<T: Stream<Item = Result<Bytes, Error>> + Unpin> {
    input: T,
    bar: ProgressBar,
    progress: u64,
    total: Option<u64>,
}

impl<T: Stream<Item = Result<Bytes, Error>> + Unpin> Stream for StreamWithProgress<T> {
    type Item = Result<Bytes, Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let next = ready!(this.input.poll_next_unpin(cx));
        if let Some(Ok(block)) = &next {
            this.bar.tick();
            this.progress += block.len() as u64;
            if let Some(total) = this.total {
                this.bar.set_message(format!(
                    "Restoring database: {}/{} processed.",
                    HumanBytes(this.progress),
                    HumanBytes(total)
                ));
            } else {
                this.bar.set_message(format!(
                    "Restoring database: {} processed.",
                    HumanBytes(this.progress)
                ));
            }
        } else {
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

pub async fn restore<'x>(
    cli: &mut Connection,
    options: &Options,
    params: &RestoreCmd,
) -> Result<(), anyhow::Error> {
    if params.all {
        restore_all(cli, options, params).await
    } else {
        restore_db(cli, options, params).await
    }
}

async fn restore_db<'x>(
    cli: &mut Connection,
    _options: &Options,
    params: &RestoreCmd,
) -> Result<(), anyhow::Error> {
    use PacketType::*;
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
    let mut buf = BytesMut::with_capacity(65536);
    let header = read_packet(&mut input, &mut buf, Header)
        .await
        .with_context(file_ctx)?
        .ok_or_else(|| anyhow::anyhow!("Dump is empty"))
        .with_context(file_ctx)?;
    let bar = ProgressBar::new_spinner();
    bar.set_message("Restoring database");
    let input = Packets {
        input: Some(input),
        buf: Some(buf),
        future: None,
    };

    let input = StreamWithProgress {
        input,
        bar,
        progress: 0,
        total: file_size,
    };

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

pub async fn restore_all<'x>(
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
        let mut db_conn = conn_params
            .connect()
            .await
            .with_context(|| format!("cannot connect to database {database:?}"))?;
        params.path = path;
        restore_db(&mut db_conn, options, &params)
            .await
            .with_context(|| format!("restoring database {database:?}"))?;
    }
    Ok(())
}
