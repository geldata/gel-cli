use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    process::{Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    task::{Context, Poll, ready},
};

use bytes::BytesMut;
use scopeguard::{self, ScopeGuard};
use serde_json::de::SliceRead;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt};

pub mod cloud;
pub mod docker;
pub mod instance;
pub mod local;

#[derive(derive_more::Error, derive_more::Display, Debug)]
#[display("Error running {:?}, {}", command, kind)]
pub struct ProcessError {
    pub kind: ProcessErrorType,
    pub command: Command,
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessErrorType {
    #[error("command failed: {0} ({1})")]
    CommandFailed(ExitStatus, String),
    #[error("encoding error: {0}")]
    EncodingError(Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<serde_json::Error> for ProcessErrorType {
    fn from(value: serde_json::Error) -> Self {
        Self::EncodingError(Box::new(value))
    }
}

impl From<std::string::FromUtf8Error> for ProcessErrorType {
    fn from(value: std::string::FromUtf8Error) -> Self {
        Self::EncodingError(Box::new(value))
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for ProcessErrorType {
    fn from(_: tokio::sync::oneshot::error::RecvError) -> Self {
        Self::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Task failed to communicate unexpectedly",
        ))
    }
}

impl From<tokio::sync::mpsc::error::TryRecvError> for ProcessErrorType {
    fn from(_: tokio::sync::mpsc::error::TryRecvError) -> Self {
        Self::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Task failed to communicate unexpectedly",
        ))
    }
}
impl From<(Command, ProcessErrorType)> for ProcessError {
    fn from((command, kind): (Command, ProcessErrorType)) -> Self {
        Self { command, kind }
    }
}

pub struct Processes<P: ProcessRunner> {
    runner: P,
}

impl<P: ProcessRunner> Processes<P> {
    pub fn new(runner: P) -> Self {
        Self { runner }
    }

    async fn run_bytes(&self, command: Command) -> Result<Vec<u8>, ProcessErrorType> {
        let (stdout, stdout_contents) = StreamCollector::entire();
        let (stderr, stderr_contents) = StreamCollector::entire();
        let handle = self
            .runner
            .run_process(command, OutputCollector { stdout, stderr })?;
        let status = handle.await?;
        if status.success() {
            let s = stdout_contents.await?;
            Ok(s)
        } else {
            let s = stderr_contents.await?;
            Err(ProcessErrorType::CommandFailed(
                status,
                String::from_utf8_lossy(&s).into_owned(),
            ))
        }
    }

    /// Run a command and decorate the error with the command.
    fn with_cmd<T, F: Future<Output = Result<T, ProcessErrorType>>>(
        command: Command,
        f: impl FnOnce(Command) -> F,
    ) -> impl Future<Output = Result<T, ProcessError>> {
        let mut cmd = Command::new(command.get_program());
        cmd.args(command.get_args());
        async move { f(command).await.map_err(|e| ProcessError::from((cmd, e))) }
    }

    pub async fn run_string(&self, command: Command) -> Result<String, ProcessError> {
        Self::with_cmd(command, |cmd| async move {
            let bytes = self.run_bytes(cmd).await?;
            Ok(String::from_utf8(bytes)?)
        })
        .await
    }

    pub async fn run_lines(
        &self,
        command: Command,
        line_handler: impl Fn(&str) + Send + Sync + 'static,
    ) -> Result<(), ProcessError> {
        Self::with_cmd(command, |cmd| async move {
            let last_output = Arc::new(Mutex::new(VecDeque::new()));
            let last_output_clone = last_output.clone();
            let line_handler = Arc::new(move |line: &str| {
                let mut lock = last_output_clone.lock().unwrap();
                lock.push_back(line.to_string());
                if lock.len() > 10 {
                    lock.pop_front();
                }
                line_handler(line);
            });
            let stdout = StreamCollector::Line(line_handler.clone());
            let stderr = StreamCollector::Line(line_handler);
            let handle = self
                .runner
                .run_process(cmd, OutputCollector { stdout, stderr })?;
            let status = handle.await?;
            if status.success() {
                Ok(())
            } else {
                let lines = std::mem::take(&mut *last_output.lock().unwrap());
                Err(ProcessErrorType::CommandFailed(
                    status,
                    lines.into_iter().collect::<Vec<_>>().join("\n"),
                ))
            }
        })
        .await
    }

    /// Run a command and return the output as a JSON value.
    #[allow(unused)]
    pub async fn run_json<T: for<'a> serde::Deserialize<'a>>(
        &self,
        command: Command,
    ) -> Result<T, ProcessError> {
        Self::with_cmd(command, |cmd| async move {
            let bytes = self.run_bytes(cmd).await?;
            Ok(serde_json::from_slice(&bytes)?)
        })
        .await
    }

    /// Run a command and return the output as a JSON array.
    pub async fn run_json_slurp<T: for<'a> serde::Deserialize<'a>>(
        &self,
        command: Command,
    ) -> Result<Vec<T>, ProcessError> {
        Self::with_cmd(command, |cmd| async move {
            let bytes = self.run_bytes(cmd).await?;
            if bytes.is_empty() {
                Ok(Vec::new())
            } else if bytes[0] == b'[' {
                Ok(serde_json::from_slice(&bytes)?)
            } else {
                Ok(serde_json::StreamDeserializer::new(SliceRead::new(&bytes))
                    .collect::<Result<Vec<_>, _>>()?)
            }
        })
        .await
    }
}

pub struct OutputCollector {
    pub stdout: StreamCollector,
    pub stderr: StreamCollector,
}

/// Collects the output of a process. These implementations are allowed to block.
#[allow(unused, clippy::type_complexity)]
pub enum StreamCollector {
    /// Ignore the stream.
    Ignore,
    /// Log the stream to `log`.
    Log,
    /// Print the stream to `stdout`.
    Print,
    /// Call a function for each line.
    Line(Arc<dyn Fn(&str) + Send + Sync + 'static>),
    /// Call a function for each chunk (each chunk is an undefined size).
    Chunk(Arc<dyn Fn(&[u8]) + Send + Sync + 'static>),
    /// Call a function for the entire stream.
    Entire(Arc<dyn Fn(Vec<u8>) + Send + Sync + 'static>),
}

impl StreamCollector {
    pub fn entire() -> (
        StreamCollector,
        impl Future<Output = Result<Vec<u8>, ProcessErrorType>>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Mutex::new(Some(tx));

        (
            StreamCollector::Entire(Arc::new(move |s| {
                if let Ok(mut tx) = tx.lock() {
                    if let Some(tx) = tx.take() {
                        _ = tx.send(s);
                    }
                }
            })),
            async move { rx.await.map_err(|e| e.into()) },
        )
    }

    pub async fn collect(self, stream: impl AsyncRead + Unpin) -> std::io::Result<()> {
        use tokio::io::BufReader;
        match self {
            StreamCollector::Ignore => {}
            StreamCollector::Log | StreamCollector::Print => {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                loop {
                    if reader.read_line(&mut line).await? == 0 {
                        break;
                    }
                    match self {
                        StreamCollector::Print => println!("{}", line),
                        StreamCollector::Log => log::info!("{}", line),
                        _ => unreachable!(),
                    }
                    line.clear();
                }
            }
            StreamCollector::Line(mut f) => {
                let mut reader = BufReader::new(stream);
                loop {
                    let mut line = vec![];
                    let bytes_read = {
                        let mut read = 0;
                        loop {
                            let available = match reader.fill_buf().await {
                                Ok(buf) => buf,
                                Err(e) => return Err(e),
                            };

                            if available.is_empty() {
                                break read; // EOF reached
                            }

                            let r_pos = memchr::memchr(b'\r', available);
                            let n_pos = memchr::memchr(b'\n', available);

                            let pos = match (r_pos, n_pos) {
                                (Some(r), Some(n)) => Some(r.min(n)),
                                (Some(r), None) => Some(r),
                                (None, Some(n)) => Some(n),
                                (None, None) => None,
                            };

                            if let Some(i) = pos {
                                line.extend_from_slice(&available[..=i]);
                                reader.consume(i + 1);
                                read += i + 1;
                                break read;
                            } else {
                                line.extend_from_slice(available);
                                let len = available.len();
                                reader.consume(len);
                                read += len;
                            }
                        }
                    };

                    if bytes_read == 0 {
                        break;
                    }
                    // Pass the function to the thread and then take it back.
                    f = tokio::task::spawn_blocking(move || {
                        for chunk in line.utf8_chunks() {
                            let valid = chunk.valid();
                            // Trim \r or \n from the end of the line.
                            let valid = valid.trim_end_matches(['\r', '\n']);
                            f(valid);
                        }
                        f
                    })
                    .await?;
                }
            }
            StreamCollector::Chunk(mut f) => {
                let mut reader = BufReader::new(stream);
                let mut buffer = BytesMut::with_capacity(1024);
                while let Ok(n) = reader.read_buf(&mut buffer).await {
                    if n == 0 {
                        break;
                    }
                    // Pass the function/buffer to the thread and then take it back.
                    (f, buffer) = tokio::task::spawn_blocking(move || {
                        f(&buffer[..n]);
                        (f, buffer)
                    })
                    .await?;
                    buffer.clear();
                }
            }
            StreamCollector::Entire(f) => {
                let mut reader = BufReader::new(stream);
                let mut buffer = vec![];
                reader.read_to_end(&mut buffer).await?;
                tokio::task::spawn_blocking(move || {
                    f(buffer);
                })
                .await?;
            }
        }
        Ok(())
    }
}

pub trait ProcessRunner {
    #[must_use = "Dropping the ProcessHandle will abort the process"]
    fn run_process(
        &self,
        command: Command,
        output: OutputCollector,
    ) -> std::io::Result<ProcessHandle>;
}

pub struct ProcessHandle {
    handle: Option<tokio::task::JoinHandle<std::io::Result<ExitStatus>>>,
}

impl Future for ProcessHandle {
    type Output = std::io::Result<ExitStatus>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(handle) = self.handle.as_mut() {
            match ready!(Pin::new(handle).poll(cx)) {
                Ok(status) => {
                    self.handle = None;
                    Poll::Ready(status)
                }
                Err(_) => {
                    self.handle = None;
                    Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "Task failed",
                    )))
                }
            }
        } else {
            Poll::Pending
        }
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

pub struct SystemProcessRunner;

impl ProcessRunner for SystemProcessRunner {
    fn run_process(
        &self,
        command: Command,
        output: OutputCollector,
    ) -> std::io::Result<ProcessHandle> {
        let mut command = tokio::process::Command::from(command);
        if !matches!(output.stdout, StreamCollector::Ignore) {
            command.stdout(Stdio::piped());
        }
        if !matches!(output.stderr, StreamCollector::Ignore) {
            command.stderr(Stdio::piped());
        }

        // Spawn the process before the task.
        let child = command.spawn()?;

        let task = tokio::task::spawn(async move {
            let mut guard = scopeguard::guard(child, |mut child| {
                _ = child.start_kill();
            });

            let mut tasks = vec![];
            if let Some(stdout) = guard.stdout.take() {
                let output = output.stdout;
                tasks.push(tokio::task::spawn(
                    async move { output.collect(stdout).await },
                ));
            }
            if let Some(stderr) = guard.stderr.take() {
                let output = output.stderr;
                tasks.push(tokio::task::spawn(
                    async move { output.collect(stderr).await },
                ));
            }
            let res = guard.wait().await;
            _ = ScopeGuard::into_inner(guard);

            if res.is_err() {
                for task in tasks {
                    task.abort();
                }
            } else {
                for task in tasks {
                    // Ignore the results of the output tasks.
                    _ = task.await?;
                }
            }

            res
        });

        Ok(ProcessHandle { handle: Some(task) })
    }
}

#[allow(unused, clippy::type_complexity)]
#[derive(Default, Debug)]
struct MockProcessRunner {
    hashmap: HashMap<String, std::io::Result<(Cow<'static, [u8]>, Cow<'static, [u8]>, ExitStatus)>>,
}

#[allow(unused)]
impl MockProcessRunner {
    pub fn new() -> Self {
        Self {
            hashmap: HashMap::new(),
        }
    }

    pub fn insert_ok(
        &mut self,
        program: impl AsRef<std::ffi::OsStr>,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
        stdout: impl AsRef<[u8]>,
        stderr: impl AsRef<[u8]>,
        status: ExitStatus,
    ) {
        let mut command = Command::new(program.as_ref());
        command.args(args);
        self.hashmap.insert(
            format!("{command:?}"),
            Ok((
                Cow::Owned(stdout.as_ref().to_vec()),
                Cow::Owned(stderr.as_ref().to_vec()),
                status,
            )),
        );
    }

    pub fn insert_err(
        &mut self,
        program: impl AsRef<std::ffi::OsStr>,
        args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
        err: std::io::ErrorKind,
    ) {
        let mut command = Command::new(program.as_ref());
        command.args(args);
        self.hashmap.insert(
            format!("{command:?}"),
            Err(std::io::Error::new(
                err,
                format!("Command failed: {command:?}"),
            )),
        );
    }
}

impl ProcessRunner for MockProcessRunner {
    fn run_process(
        &self,
        command: Command,
        output: OutputCollector,
    ) -> std::io::Result<ProcessHandle> {
        let command_string = format!("{command:?}");
        match self
            .hashmap
            .get(&command_string)
            .unwrap_or_else(|| panic!("Command not found: {command_string}"))
        {
            Ok((stdout, stderr, status)) => {
                (stdout.to_vec().into(), stderr.to_vec().into(), *status)
                    .run_process(command, output)
            }
            Err(e) => Err(std::io::Error::new(
                e.kind(),
                format!("Command failed: {command_string}"),
            )),
        }
    }
}

impl ProcessRunner for (Cow<'static, [u8]>, Cow<'static, [u8]>, ExitStatus) {
    fn run_process(
        &self,
        _command: Command,
        output: OutputCollector,
    ) -> std::io::Result<ProcessHandle> {
        let (stdout, stderr, status) = (self.0.to_vec(), self.1.to_vec(), self.2);
        let task = tokio::task::spawn(async move {
            let mut tasks = vec![];
            let (err_r, mut err_w) = tokio::io::duplex(1024);
            let (out_r, mut out_w) = tokio::io::duplex(1024);
            tasks.push(tokio::task::spawn(async move {
                for chunk in stdout.chunks(1024) {
                    _ = out_w.write_all(chunk).await;
                }
            }));
            tasks.push(tokio::task::spawn(async move {
                for chunk in stderr.chunks(1024) {
                    _ = err_w.write_all(chunk).await;
                }
            }));
            let stdout = output.stdout;
            tasks.push(tokio::task::spawn(async move {
                _ = stdout.collect(out_r).await;
            }));
            let stderr = output.stderr;
            tasks.push(tokio::task::spawn(async move {
                _ = stderr.collect(err_r).await;
            }));
            for task in tasks {
                task.await?;
            }
            Ok(status)
        });
        Ok(ProcessHandle { handle: Some(task) })
    }
}

/// Mock process runner that runs a command and returns the output as a string.
impl<F, SO: Into<Cow<'static, [u8]>>, SE: Into<Cow<'static, [u8]>>> ProcessRunner for F
where
    F: Fn(&Command) -> std::io::Result<(SO, SE, ExitStatus)> + Send + Sync + 'static,
{
    fn run_process(
        &self,
        command: Command,
        output: OutputCollector,
    ) -> std::io::Result<ProcessHandle> {
        let res = (self)(&command)?;
        (res.0.into(), res.1.into(), res.2).run_process(command, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_process_runner() {
        let runner = Box::new(&|cmd: &Command| {
            let exe = cmd.get_program().to_string_lossy().into_owned();
            Ok((
                json!({"command": exe}).to_string().into_bytes(),
                "".as_bytes(),
                ExitStatus::default(),
            ))
        });

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = runner
            .run_process(
                Command::new("hi"),
                OutputCollector {
                    stdout: StreamCollector::Entire(Arc::new(move |s| {
                        tx.send(s.to_vec()).unwrap()
                    })),
                    stderr: StreamCollector::Log,
                },
            )
            .unwrap();
        assert_eq!(handle.await.unwrap(), ExitStatus::default());
        let stdout = rx.recv().await.unwrap();
        assert_eq!(stdout, br#"{"command":"hi"}"#);

        let processes = Processes::new(runner);
        let json = processes
            .run_json::<serde_json::Value>(Command::new("echo"))
            .await
            .unwrap();
        assert_eq!(json, json!({"command": "echo"}));
    }

    #[tokio::test]
    async fn test_run_lines() {
        let runner = Box::new(&|_: &Command| {
            Ok((
                "a\rb\rc\rd".as_bytes(),
                "".as_bytes(),
                ExitStatus::default(),
            ))
        });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = runner
            .run_process(
                Command::new("hi"),
                OutputCollector {
                    stdout: StreamCollector::Line(Arc::new(move |s| {
                        tx.send(s.to_string()).unwrap()
                    })),
                    stderr: StreamCollector::Log,
                },
            )
            .unwrap();
        assert_eq!(handle.await.unwrap(), ExitStatus::default());

        for line in ["a", "b", "c", "d"] {
            let stdout = rx.recv().await.unwrap();
            assert_eq!(stdout, line);
        }
    }
}
