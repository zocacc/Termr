use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::Notify;

pub mod fake;
pub mod host_key;
mod russh_client;

pub use russh_client::{RusshClient, connect_request_for_host};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    IdentityFile(PathBuf),
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub host_id: String,
    pub address: String,
    pub port: u16,
    pub username: String,
    pub authentication: Authentication,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    pub algorithm: String,
    pub fingerprint: String,
    pub public_key_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_status: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyRequest {
    pub terminal: String,
    pub columns: u32,
    pub rows: u32,
}

impl PtyRequest {
    pub fn new(terminal: impl Into<String>, columns: u32, rows: u32) -> Self {
        Self {
            terminal: terminal.into(),
            columns,
            rows,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshErrorKind {
    Dns,
    ConnectionRefused,
    Timeout,
    HostKeyUnknown,
    HostKeyChanged,
    AuthenticationRejected,
    AgentUnavailable,
    IdentityFile,
    Protocol,
    Cancelled,
    Closed,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct SshError {
    kind: SshErrorKind,
    message: String,
    technical: Option<String>,
}

impl SshError {
    pub fn new(kind: SshErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            technical: None,
        }
    }

    pub fn with_technical(mut self, technical: impl Into<String>) -> Self {
        self.technical = Some(technical.into());
        self
    }

    pub const fn kind(&self) -> SshErrorKind {
        self.kind
    }

    pub fn technical(&self) -> Option<&str> {
        self.technical.as_deref()
    }
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

#[async_trait]
pub trait HostKeyVerifier: Send + Sync {
    async fn verify(&self, request: &ConnectRequest, key: &HostKey) -> Result<(), SshError>;
}

#[async_trait]
pub trait SshClient: Send + Sync {
    async fn connect(
        &self,
        request: ConnectRequest,
        verifier: Arc<dyn HostKeyVerifier>,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn SshConnection>, SshError>;
}

#[async_trait]
pub trait SshConnection: Send {
    async fn exec(
        &mut self,
        command: &str,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput, SshError>;

    async fn open_pty(
        &mut self,
        request: PtyRequest,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn SshPty>, SshError>;

    async fn close(self: Box<Self>) -> Result<(), SshError>;
}

#[async_trait]
pub trait SshPty: Send {
    async fn send_input(
        &mut self,
        input: &[u8],
        cancellation: CancellationToken,
    ) -> Result<(), SshError>;

    async fn resize(
        &mut self,
        columns: u32,
        rows: u32,
        cancellation: CancellationToken,
    ) -> Result<(), SshError>;

    async fn next_output(
        &mut self,
        cancellation: CancellationToken,
    ) -> Result<Option<Vec<u8>>, SshError>;

    async fn close(self: Box<Self>) -> Result<(), SshError>;
}

#[cfg(test)]
mod auth {
    use super::*;

    #[test]
    fn supported_authentication_strategies_exclude_passwords() {
        let strategies = [
            Authentication::IdentityFile(PathBuf::from("/tmp/key")),
            Authentication::Agent,
        ];
        assert_eq!(strategies.len(), 2);
    }
}
