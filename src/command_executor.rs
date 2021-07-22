//!
//! Process command creation and execution
//!
use std::cell::{Cell, RefCell};
use std::error::Error;
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::marker;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use futures::TryFutureExt;
use log;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader, Lines};
use tokio::process::{Child, ChildStdout};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::JoinHandle;
use tokio::time::Duration;

///
/// Output logging type
///
#[derive(Debug)]
pub enum LogType {
    Info,
    Error,
}

///
/// Child process status
///
pub trait ProcessStatus<T, E>
where
    E: Error + Send,
    Self: Send,
{
    /// process entry status
    fn status_entry(&self) -> T;
    /// process exit status
    fn status_exit(&self) -> T;
    /// process error type
    fn error_type(&self) -> E;
    /// wrap error
    fn wrap_error<F: Error + Send + 'static>(&self, error: F) -> E;
}

///
/// Logging data
///
#[derive(Debug)]
pub struct LogOutputData {
    line: String,
    log_type: LogType,
}

///
/// Async command trait
///
#[async_trait]
pub trait AsyncCommand<S, E, P>
where
    E: Error + Send,
    P: ProcessStatus<S, E> + Send,
    Self: Sized,
{
    ///
    /// Create a new async command
    ///
    fn new<A, B>(executable_path: &OsStr, args: A, process_type: P) -> Result<Self, E>
    where
        A: IntoIterator<Item = B>,
        B: AsRef<OsStr>;
    ///
    /// Execute command
    ///
    /// When timeout is Some(duration) the process execution will be timed out after duration,
    /// if set to None the process execution will not be timed out.
    ///
    async fn execute(&mut self, timeout: Option<Duration>) -> Result<S, E>;
}

///
/// Process command
///
pub struct AsyncCommandExecutor<S, E, P>
where
    S: Send,
    E: Error + Send,
    P: ProcessStatus<S, E>,
    Self: Send,
{
    /// Process command
    command: tokio::process::Command,
    /// Process child
    process: Child,
    /// Process type
    process_type: P,
    _marker_s: marker::PhantomData<S>,
    _marker_e: marker::PhantomData<E>,
}

impl<S, E, P> AsyncCommandExecutor<S, E, P>
where
    S: Send,
    E: Error + Send,
    P: ProcessStatus<S, E> + Send,
{
    /// Initialize command
    fn init(command: &mut tokio::process::Command, process_type: &P) -> Result<Child, E> {
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| process_type.error_type())
    }

    /// Generate a command
    fn generate_command<A, B>(executable_path: &OsStr, args: A) -> tokio::process::Command
    where
        A: IntoIterator<Item = B>,
        B: AsRef<OsStr>,
    {
        let mut command = tokio::process::Command::new(executable_path);
        command.args(args);
        command
    }

    /// Handle process output
    async fn handle_output<R: AsyncRead + Unpin>(data: R, sender: Sender<LogOutputData>) -> () {
        let mut lines = BufReader::new(data).lines();
        while let Some(line) = lines.next_line().await.expect("error handling output") {
            let io_data = LogOutputData {
                line,
                log_type: LogType::Info,
            };
            sender
                .send(io_data)
                .await
                .expect("error sending log output data");
        }
    }

    /// Log process output
    async fn log_output(mut receiver: Receiver<LogOutputData>) -> () {
        while let Some(data) = receiver.recv().await {
            match data.log_type {
                LogType::Info => {
                    log::info!("{}", data.line);
                }
                LogType::Error => {
                    log::error!("{}", data.line);
                }
            }
        }
    }

    /// Run process
    async fn run_process(&mut self) -> Result<S, E> {
        let exit_status = self
            .process
            .wait()
            .await
            .map_err(|e| self.process_type.wrap_error(e))?;
        if exit_status.success() {
            Ok(self.process_type.status_exit())
        } else {
            Err(self.process_type.error_type())
        }
    }
}

#[async_trait]
impl<S, E, P> AsyncCommand<S, E, P> for AsyncCommandExecutor<S, E, P>
where
    S: Send,
    E: Error + Send,
    P: ProcessStatus<S, E> + Send,
{
    fn new<A, B>(executable_path: &OsStr, args: A, process_type: P) -> Result<Self, E>
    where
        A: IntoIterator<Item = B>,
        B: AsRef<OsStr>,
    {
        let mut command = Self::generate_command(executable_path, args);
        let process = Self::init(&mut command, &process_type)?;
        Ok(AsyncCommandExecutor {
            command,
            process,
            process_type,
            _marker_s: Default::default(),
            _marker_e: Default::default(),
        })
    }

    async fn execute(&mut self, timeout: Option<Duration>) -> Result<S, E> {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<LogOutputData>(1000);
        {
            let tx = sender.clone();
            let stdout = self.process.stdout.take().unwrap();
            let _ = tokio::spawn(async move { Self::handle_output(stdout, tx).await });
        }
        {
            let stderr = self.process.stderr.take().unwrap();
            let _ = tokio::spawn(async move { Self::handle_output(stderr, sender).await });
        }
        Self::log_output(receiver).await;
        self.run_process().await
    }
}
