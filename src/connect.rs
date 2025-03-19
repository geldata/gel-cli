use std::borrow::Cow;
use std::future::{Future, pending};
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;

use gel_dsn::gel::DatabaseBranch;
use tokio::time::sleep;
use tokio_stream::Stream;

use gel_errors::{ClientError, NoDataError, ProtocolEncodingError};
use gel_errors::{Error, ErrorKind, ResultExt};
use gel_protocol::QueryResult;
use gel_protocol::annotations::Warning;
use gel_protocol::client_message::{CompilationOptions, State};
use gel_protocol::common::{Capabilities, Cardinality, IoFormat};
use gel_protocol::descriptors::{RawTypedesc, Typedesc};
use gel_protocol::encoding::Annotations;
use gel_protocol::features::ProtocolVersion;
use gel_protocol::model::Uuid;
use gel_protocol::query_arg::QueryArgs;
use gel_protocol::server_message::CommandDataDescription1;
use gel_protocol::server_message::RawPacket;
use gel_protocol::server_message::TransactionState;
use gel_protocol::value::Value;
use gel_tokio::Config;
use gel_tokio::raw::{self, PoolState, Response};
use gel_tokio::server_params::ServerParam;

use crate::branding::{BRANDING, BRANDING_CLOUD, QUERY_TAG, REPL_QUERY_TAG};
use crate::hint::ArcError;
use crate::portable::ver;

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("Connection error: {0}")]
    Error(Error),
    #[error("I/O error. Check your connection to the database: {0}")]
    IoError(Error),
    #[error(
        "Permission error. This is usually caused by a firewall. Try disabling \
        your OS's firewall or any other firewalls you have installed"
    )]
    PermissionError(Error),
}

#[derive(Debug, Clone)]
pub struct Connector {
    config: Result<Config, ArcError>,
}

pub struct Connection {
    inner: raw::Connection,
    server_version: Option<ver::Build>,
    state: State,
    config: Config,
    annotations: Arc<Annotations>,
}

pub struct ResponseStream<'a, T: QueryResult>
where
    T::State: Unpin,
{
    inner: raw::ResponseStream<'a, T>,
    state: &'a mut State,
}

pub struct DumpStream<'a> {
    inner: raw::DumpStream<'a>,
    state: &'a mut State,
}

fn update_state<T>(state: &mut State, resp: &raw::Response<T>) -> Result<(), Error> {
    if let Some(raw_state) = &resp.new_state {
        *state = raw_state.clone();
    }
    Ok(())
}

impl<'a, T: QueryResult> ResponseStream<'a, T>
where
    T::State: Unpin,
{
    pub fn can_contain_data(&self) -> bool {
        self.inner.can_contain_data()
    }
    pub async fn next_element(&mut self) -> Option<T> {
        self.inner.next_element().await
    }
    pub async fn complete(mut self) -> Result<Response<()>, Error> {
        let resp = self.inner.process_complete().await?;
        update_state(self.state, &resp)?;
        Ok(resp)
    }
    async fn next(&mut self) -> Option<Result<T, Error>> {
        self.next_element().await.map(Ok)
    }
    pub fn warnings(&self) -> &[Warning] {
        self.inner.warnings()
    }
}

impl<'a, T: QueryResult> Stream for ResponseStream<'a, T>
where
    T::State: Unpin,
{
    type Item = Result<T, Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<T, Error>>> {
        let next = self.get_mut().next();
        tokio::pin!(next);
        next.poll(cx)
    }
}

impl DumpStream<'_> {
    async fn next(&mut self) -> Option<Result<RawPacket, Error>> {
        if let Some(el) = self.inner.next_block().await {
            Some(Ok(el))
        } else {
            match self.inner.process_complete().await {
                Ok(resp) => match update_state(self.state, &resp) {
                    Ok(()) => None,
                    Err(e) => Some(Err(e)),
                },
                Err(e) => Some(Err(e)),
            }
        }
    }
}

impl Stream for DumpStream<'_> {
    type Item = Result<RawPacket, Error>;
    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<RawPacket, Error>>> {
        let next = self.get_mut().next();
        tokio::pin!(next);
        next.poll(cx)
    }
}

impl Connector {
    pub fn new(config: anyhow::Result<Config>) -> Connector {
        Connector {
            config: config.map_err(ArcError::from),
        }
    }
    pub fn branch(&mut self, name: &str) -> anyhow::Result<&mut Self> {
        if let Ok(cfg) = self.config.as_mut() {
            if name == "__default__" {
                *cfg = cfg.with_db(DatabaseBranch::Default);
            } else {
                *cfg = cfg.with_db(DatabaseBranch::Ambiguous(name.to_string()));
            }
        }
        Ok(self)
    }
    pub fn db(&mut self, db: DatabaseBranch) -> anyhow::Result<&mut Self> {
        if let Ok(cfg) = self.config.as_mut() {
            *cfg = cfg.with_db(db);
        }
        Ok(self)
    }
    pub fn wait_until_available(&mut self, dur: Duration) -> &mut Self {
        if let Ok(cfg) = self.config.as_mut() {
            *cfg = cfg.clone().with_wait_until_available(dur);
        }
        self
    }
    pub async fn connect(&self) -> Result<Connection, anyhow::Error> {
        self._connect(false).await
    }

    pub async fn connect_interactive(&self) -> Result<Connection, anyhow::Error> {
        self._connect(true).await
    }

    async fn _connect(&self, interactive: bool) -> Result<Connection, anyhow::Error> {
        let cfg = self.config.as_ref().map_err(Clone::clone)?;
        let tag = if interactive {
            REPL_QUERY_TAG
        } else {
            QUERY_TAG
        };
        let conn = tokio::select!(
            conn = Connection::connect(cfg, tag) => {
                if interactive { eprintln!() }
                conn?
            },
            _ = self.print_warning(cfg, interactive) => unreachable!(),
        );
        Ok(conn)
    }

    fn warning_msg(&self, cfg: &Config) -> String {
        let desc = match cfg.instance_name() {
            Some(gel_tokio::InstanceName::Cloud(cloud_name)) => {
                format!("{BRANDING_CLOUD} instance '{cloud_name}'")
            }
            Some(gel_tokio::InstanceName::Local(name)) => {
                format!("{BRANDING} instance '{}' at {}", name, cfg.display_addr())
            }
            _ => format!("{BRANDING} instance at {}", cfg.display_addr()),
        };
        format!("Connecting to {desc}...")
    }

    async fn print_warning(&self, cfg: &Config, interactive: bool) -> Result<Connection, Error> {
        sleep(Duration::new(1, 0)).await;
        let msg = self.warning_msg(cfg);
        if interactive {
            eprint!("{msg}");
        } else {
            eprintln!("{msg}");
        }
        pending().await
    }
    pub fn get(&self) -> anyhow::Result<&Config, ArcError> {
        self.config.as_ref().map_err(Clone::clone)
    }

    pub async fn run_single_query<R>(self, query: &str) -> Result<Vec<R>, anyhow::Error>
    where
        R: QueryResult,
    {
        let mut connection = self.connect().await?;
        let results = connection.query(query, &()).await?;
        Ok(results)
    }
}

impl Connection {
    pub fn from_raw(cfg: &Config, raw: raw::Connection, tag: impl ToString) -> Connection {
        let mut annotations = Annotations::new();
        annotations.insert("tag".to_string(), tag.to_string());
        Connection {
            inner: raw,
            state: State::empty(),
            server_version: None,
            config: cfg.clone(),
            annotations: Arc::new(Annotations::new()),
        }
    }

    pub async fn connect(cfg: &Config, tag: impl ToString) -> Result<Connection, ConnectionError> {
        let mut annotations = Annotations::new();
        annotations.insert("tag".to_string(), tag.to_string());
        Ok(Connection {
            inner: raw::Connection::connect(cfg)
                .await
                .map_err(Self::map_connection_err)?,
            state: State::empty(),
            server_version: None,
            config: cfg.clone(),
            annotations: Arc::new(annotations),
        })
    }

    fn map_connection_err(err: Error) -> ConnectionError {
        let mut source_walker: &dyn std::error::Error = &err;

        while let Some(source) = source_walker.source() {
            if let Some(io_error) = source.downcast_ref::<std::io::Error>() {
                let kind = io_error.kind();
                if kind == std::io::ErrorKind::PermissionDenied {
                    return ConnectionError::PermissionError(err);
                }
                let message = io_error.to_string();
                return ConnectionError::IoError(err.context(message));
            }
            source_walker = source;
        }

        ConnectionError::Error(err)
    }

    pub fn database(&self) -> &DatabaseBranch {
        &self.config.db
    }
    pub fn set_ignore_error_state(&mut self) -> State {
        let new_state = make_ignore_error_state(self.inner.state_descriptor());
        mem::replace(&mut self.state, new_state)
    }
    pub fn restore_state(&mut self, state: State) {
        self.state = state;
    }
    pub async fn get_version(&mut self) -> Result<&ver::Build, Error> {
        if self.server_version.is_some() {
            return Ok(self.server_version.as_ref().unwrap());
        }
        let state = make_ignore_error_state(self.inner.state_descriptor());
        let resp: String = self
            .inner
            .query(
                "SELECT sys::get_version_as_str()",
                &(),
                &state,
                &self.annotations,
                Capabilities::empty(),
                IoFormat::Binary,
                Cardinality::AtMostOne,
            )
            .await
            .map(|x| x.data.into_iter().next().unwrap_or_default())
            .context("cannot fetch database version")?;
        let build = resp.parse()?;
        Ok(self.server_version.insert(build))
    }
    pub async fn get_current_branch(&mut self) -> Result<Cow<'_, str>, Error> {
        if let Some(name) = self.config.db.name() {
            return Ok(name.into());
        }

        let state = make_ignore_error_state(self.inner.state_descriptor());
        let resp: raw::Response<Vec<String>> = self
            .inner
            .query(
                "SELECT sys::get_current_database()",
                &(),
                &state,
                &self.annotations,
                Capabilities::empty(),
                IoFormat::Binary,
                Cardinality::AtMostOne,
            )
            .await
            .context("cannot fetch current database branch")?;
        let branch = resp.data.into_iter().next().unwrap_or_default();
        Ok(branch.into())
    }
    pub async fn query<R, A>(&mut self, query: &str, arguments: &A) -> Result<Vec<R>, Error>
    where
        A: QueryArgs,
        R: QueryResult,
    {
        let resp = self
            .inner
            .query(
                query,
                arguments,
                &self.state,
                &self.annotations,
                Capabilities::ALL,
                IoFormat::Binary,
                Cardinality::Many,
            )
            .await?;
        update_state(&mut self.state, &resp)?;
        Ok(resp.data)
    }
    pub async fn query_single<R, A>(
        &mut self,
        query: &str,
        arguments: &A,
    ) -> Result<(Option<R>, Vec<Warning>), Error>
    where
        A: QueryArgs,
        R: QueryResult,
    {
        let resp = self
            .inner
            .query(
                query,
                arguments,
                &self.state,
                &self.annotations,
                Capabilities::ALL,
                IoFormat::Binary,
                Cardinality::AtMostOne,
            )
            .await?;
        update_state(&mut self.state, &resp)?;
        let data = resp.data.into_iter().next();
        Ok((data, resp.warnings))
    }
    pub async fn query_required_single<R, A>(
        &mut self,
        query: &str,
        arguments: &A,
    ) -> Result<R, Error>
    where
        A: QueryArgs,
        R: QueryResult,
    {
        let (res, _) = self.query_single(query, arguments).await?;
        res.ok_or_else(|| NoDataError::with_message("query row returned zero results"))
    }
    pub async fn execute<A>(
        &mut self,
        query: &str,
        arguments: &A,
    ) -> Result<(Bytes, Vec<Warning>), Error>
    where
        A: QueryArgs,
    {
        let resp = self
            .inner
            .execute(
                query,
                arguments,
                &self.state,
                &self.annotations,
                Capabilities::ALL,
            )
            .await?;
        update_state(&mut self.state, &resp)?;
        Ok((resp.status_data, resp.warnings))
    }
    pub async fn execute_stream<R, A>(
        &mut self,
        opts: &CompilationOptions,
        query: &str,
        desc: &CommandDataDescription1,
        arguments: &A,
    ) -> Result<ResponseStream<R>, Error>
    where
        A: QueryArgs,
        R: QueryResult,
        R::State: Unpin,
    {
        let stream = self
            .inner
            .execute_stream(opts, query, &self.state, &self.annotations, desc, arguments)
            .await?;
        Ok(ResponseStream {
            inner: stream,
            state: &mut self.state,
        })
    }
    pub async fn try_execute_stream<R, A>(
        &mut self,
        opts: &CompilationOptions,
        query: &str,
        input_desc: &Typedesc,
        output_desc: &Typedesc,
        arguments: &A,
    ) -> Result<ResponseStream<R>, Error>
    where
        A: QueryArgs,
        R: QueryResult,
        R::State: Unpin,
    {
        let stream = self
            .inner
            .try_execute_stream(
                opts,
                query,
                &self.state,
                &self.annotations,
                input_desc,
                output_desc,
                arguments,
            )
            .await?;
        Ok(ResponseStream {
            inner: stream,
            state: &mut self.state,
        })
    }
    pub fn get_server_param<T: ServerParam>(&self) -> Option<&T::Value> {
        self.inner.get_server_param::<T>()
    }
    pub fn is_consistent(&self) -> bool {
        self.inner.is_consistent()
    }
    pub async fn ping_while<T, F>(&mut self, other: F) -> T
    where
        F: Future<Output = T>,
    {
        self.inner.ping_while(other).await
    }
    pub async fn terminate(self) -> Result<(), Error> {
        self.inner.terminate().await
    }
    pub fn protocol(&self) -> &ProtocolVersion {
        self.inner.protocol()
    }
    pub fn transaction_state(&self) -> TransactionState {
        self.inner.transaction_state()
    }
    pub fn get_state_as_value(&self) -> Result<(Uuid, Value), Error> {
        if self.state.typedesc_id == Uuid::from_u128(0) {
            return Ok((Uuid::from_u128(0), Value::Nothing));
        }
        let desc = self.inner.state_descriptor();
        if desc.id != self.state.typedesc_id {
            return Err(ClientError::with_message(format!(
                "State type descriptor id is {:?}, \
                             but state is encoded using {:?}",
                desc.id, self.state.typedesc_id
            )));
        }
        let desc = desc.decode().map_err(ProtocolEncodingError::with_source)?;
        let codec = desc
            .build_codec()
            .map_err(ProtocolEncodingError::with_source)?;
        let value = codec
            .decode(&self.state.data[..])
            .map_err(ProtocolEncodingError::with_source)?;

        Ok((*desc.id(), value))
    }
    pub fn get_state(&self) -> &State {
        &self.state
    }
    pub fn set_state(&mut self, state: State) {
        self.state = state;
    }
    pub fn get_state_desc(&self) -> RawTypedesc {
        self.inner.state_descriptor().clone()
    }
    pub async fn parse(
        &mut self,
        opts: &CompilationOptions,
        query: &str,
    ) -> Result<CommandDataDescription1, Error> {
        self.inner
            .parse(opts, query, &self.state, &self.annotations)
            .await
    }
    pub async fn restore(
        &mut self,
        header: Bytes,
        stream: impl Stream<Item = Result<Bytes, Error>> + Unpin,
    ) -> Result<(), Error> {
        let resp = self.inner.restore(header, stream).await?;
        update_state(&mut self.state, &resp)?;
        Ok(())
    }
    pub async fn dump(
        &mut self,
        include_secrets: bool,
    ) -> Result<(RawPacket, impl Stream<Item = Result<RawPacket, Error>> + '_), Error> {
        let mut inner = self.inner.dump_with_secrets(include_secrets).await?;
        let header = inner.take_header().expect("header is read");
        let stream = DumpStream {
            inner,
            state: &mut self.state,
        };
        Ok((header, stream))
    }

    pub fn set_tag(&mut self, tag: impl ToString) {
        let mut annotations = (*self.annotations).clone();
        annotations.insert("tag".to_string(), tag.to_string());
        self.annotations = Arc::new(annotations);
    }
}

fn make_ignore_error_state(desc: &RawTypedesc) -> State {
    _make_ignore_error_state(desc).unwrap_or(State::empty())
}

#[derive(gel_derive::ConfigDelta)]
struct ErrorState {
    force_database_error: &'static str,
}

fn _make_ignore_error_state(desc: &RawTypedesc) -> Option<State> {
    PoolState::default()
        .with_config(&ErrorState {
            force_database_error: "false",
        })
        .encode(desc)
        .ok()
}
