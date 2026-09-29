use std::future::Future;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use directories::BaseDirs;
use russh::Disconnect;
use russh::client;
use russh::keys::PrivateKeyWithHashAlg;

use crate::inventory::{AuthMethod, Host};

use super::{
    Authentication, CancellationToken, CommandOutput, ConnectRequest, HostKey, HostKeyVerifier,
    PtyRequest, SshClient, SshConnection, SshError, SshErrorKind, SshPty,
};

#[derive(Debug, Clone, Default)]
pub struct RusshClient;

pub fn connect_request_for_host(
    host: &Host,
    timeout: Duration,
) -> Result<ConnectRequest, SshError> {
    let authentication = match host.auth_method {
        AuthMethod::Agent => Authentication::Agent,
        AuthMethod::IdentityFile => {
            let path = host
                .identity_file
                .as_deref()
                .ok_or_else(|| identity_error("The host has no SSH identity file configured."))?;
            Authentication::IdentityFile(expand_identity_path(path)?)
        }
    };
    Ok(ConnectRequest {
        host_id: host.id.clone(),
        address: host.address.clone(),
        port: host.port,
        username: host.username.clone(),
        authentication,
        timeout,
    })
}

pub fn expand_identity_path(path: &Path) -> Result<PathBuf, SshError> {
    let home = BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .ok_or_else(|| identity_error("The home directory could not be determined."))?;
    expand_identity_path_from(path, &home)
}

fn expand_identity_path_from(path: &Path, home: &Path) -> Result<PathBuf, SshError> {
    let value = path.to_string_lossy();
    if value == "~" {
        return Ok(home.to_path_buf());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return Ok(home.join(rest));
    }
    if value.starts_with('~') {
        return Err(identity_error(
            "Identity paths may use '~/' but not another user's home directory.",
        ));
    }
    Ok(path.to_path_buf())
}

struct ClientHandler {
    request: ConnectRequest,
    verifier: Arc<dyn HostKeyVerifier>,
    verification_error: Arc<Mutex<Option<SshError>>>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let public_key = server_key.public_key();
        let openssh = public_key.to_openssh().map_err(russh::Error::SshKey)?;
        let key = HostKey {
            algorithm: public_key.algorithm().to_string(),
            fingerprint: public_key
                .fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
                .to_string(),
            public_key_base64: openssh
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned(),
        };
        match self.verifier.verify(&self.request, &key).await {
            Ok(()) => Ok(true),
            Err(error) => {
                *self.verification_error.lock().expect("mutex poisoned") = Some(error);
                Ok(false)
            }
        }
    }
}

#[async_trait]
impl SshClient for RusshClient {
    async fn connect(
        &self,
        request: ConnectRequest,
        verifier: Arc<dyn HostKeyVerifier>,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn SshConnection>, SshError> {
        let timeout = request.timeout;
        run_with_deadline(
            timeout,
            cancellation,
            self.connect_and_authenticate(request, verifier),
        )
        .await
    }
}

async fn run_with_deadline<T, F>(
    timeout: Duration,
    cancellation: CancellationToken,
    operation: F,
) -> Result<T, SshError>
where
    F: Future<Output = Result<T, SshError>>,
{
    tokio::select! {
        _ = cancellation.cancelled() => Err(cancelled_error()),
        result = tokio::time::timeout(timeout, operation) => {
            result.unwrap_or_else(|_| Err(timeout_error()))
        },
    }
}

impl RusshClient {
    async fn connect_and_authenticate(
        &self,
        request: ConnectRequest,
        verifier: Arc<dyn HostKeyVerifier>,
    ) -> Result<Box<dyn SshConnection>, SshError> {
        let verification_error = Arc::new(Mutex::new(None));
        let handler = ClientHandler {
            request: request.clone(),
            verifier,
            verification_error: Arc::clone(&verification_error),
        };
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(request.timeout),
            ..Default::default()
        });
        let mut handle = match client::connect(
            config,
            (request.address.as_str(), request.port),
            handler,
        )
        .await
        {
            Err(error) => {
                if let Some(error) = verification_error.lock().expect("mutex poisoned").take() {
                    return Err(error);
                }
                return Err(map_russh_error(error));
            }
            Ok(handle) => handle,
        };

        let mut authenticator = RusshAuthenticator {
            handle: &mut handle,
        };
        let authenticated = authenticate(
            &mut authenticator,
            &request.username,
            &request.authentication,
        )
        .await?;
        if !authenticated {
            return Err(authentication_rejected_error());
        }
        Ok(Box::new(RusshConnection { handle }))
    }
}

#[async_trait]
trait AuthenticationDriver: Send {
    async fn identity_file(&mut self, username: &str, path: &Path) -> Result<bool, SshError>;
    async fn agent(&mut self, username: &str) -> Result<bool, SshError>;
}

struct RusshAuthenticator<'a> {
    handle: &'a mut client::Handle<ClientHandler>,
}

#[async_trait]
impl AuthenticationDriver for RusshAuthenticator<'_> {
    async fn identity_file(&mut self, username: &str, path: &Path) -> Result<bool, SshError> {
        authenticate_identity(self.handle, username, path).await
    }

    async fn agent(&mut self, username: &str) -> Result<bool, SshError> {
        authenticate_agent(self.handle, username).await
    }
}

async fn authenticate(
    driver: &mut dyn AuthenticationDriver,
    username: &str,
    authentication: &Authentication,
) -> Result<bool, SshError> {
    match authentication {
        Authentication::IdentityFile(path) => driver.identity_file(username, path).await,
        Authentication::Agent => driver.agent(username).await,
    }
}

async fn authenticate_identity(
    handle: &mut client::Handle<ClientHandler>,
    username: &str,
    path: &Path,
) -> Result<bool, SshError> {
    let key = russh::keys::load_secret_key(path, None).map_err(|error| {
        identity_error("The SSH identity file could not be loaded.")
            .with_technical(error.to_string())
    })?;
    let hash = rsa_hash(handle).await?;
    handle
        .authenticate_publickey(
            username.to_owned(),
            PrivateKeyWithHashAlg::new(Arc::new(key), hash),
        )
        .await
        .map(|result| result.success())
        .map_err(map_russh_error)
}

#[cfg(unix)]
async fn authenticate_agent(
    handle: &mut client::Handle<ClientHandler>,
    username: &str,
) -> Result<bool, SshError> {
    let mut agent = russh::keys::agent::client::AgentClient::connect_env()
        .await
        .map_err(|error| agent_error(error.to_string()))?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|error| agent_error(error.to_string()))?;
    if identities.is_empty() {
        return Err(agent_error("SSH Agent has no identities"));
    }
    let hash = rsa_hash(handle).await?;
    for identity in identities {
        let result = handle
            .authenticate_publickey_with(
                username.to_owned(),
                identity.public_key().into_owned(),
                hash,
                &mut agent,
            )
            .await
            .map_err(map_agent_auth_error)?;
        if result.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn map_agent_auth_error(error: russh::AgentAuthError) -> SshError {
    match error {
        russh::AgentAuthError::Send(error) => SshError::new(
            SshErrorKind::Protocol,
            "The SSH connection closed during Agent authentication.",
        )
        .with_technical(error.to_string()),
        russh::AgentAuthError::Key(error) => agent_error(error.to_string()),
    }
}

async fn rsa_hash(
    handle: &client::Handle<ClientHandler>,
) -> Result<Option<russh::keys::ssh_key::HashAlg>, SshError> {
    handle
        .best_supported_rsa_hash()
        .await
        .map_err(map_russh_error)
        .map(Option::flatten)
}

#[cfg(not(unix))]
async fn authenticate_agent(
    _handle: &mut client::Handle<ClientHandler>,
    _username: &str,
) -> Result<bool, SshError> {
    Err(agent_error("SSH Agent is unsupported on this platform"))
}

struct RusshConnection {
    handle: client::Handle<ClientHandler>,
}

#[async_trait]
impl SshConnection for RusshConnection {
    async fn exec(
        &mut self,
        _command: &str,
        _cancellation: CancellationToken,
    ) -> Result<CommandOutput, SshError> {
        Err(not_implemented("Remote commands"))
    }

    async fn open_pty(
        &mut self,
        _request: PtyRequest,
        _cancellation: CancellationToken,
    ) -> Result<Box<dyn SshPty>, SshError> {
        Err(not_implemented("Interactive sessions"))
    }

    async fn close(mut self: Box<Self>) -> Result<(), SshError> {
        self.handle
            .disconnect(Disconnect::ByApplication, "Termr closed the session", "en")
            .await
            .map_err(map_russh_error)
    }
}

fn map_russh_error(error: russh::Error) -> SshError {
    let kind = match &error {
        russh::Error::IO(io) if io.kind() == ErrorKind::ConnectionRefused => {
            SshErrorKind::ConnectionRefused
        }
        russh::Error::IO(io)
            if matches!(io.kind(), ErrorKind::NotFound | ErrorKind::AddrNotAvailable) =>
        {
            SshErrorKind::Dns
        }
        russh::Error::ConnectionTimeout
        | russh::Error::KeepaliveTimeout
        | russh::Error::InactivityTimeout
        | russh::Error::Elapsed(_) => SshErrorKind::Timeout,
        _ => SshErrorKind::Protocol,
    };
    let message = match kind {
        SshErrorKind::ConnectionRefused => "The SSH server refused the connection.",
        SshErrorKind::Dns => "The SSH host name could not be resolved.",
        SshErrorKind::Timeout => "The SSH connection timed out.",
        _ => "The SSH connection failed.",
    };
    SshError::new(kind, message).with_technical(error.to_string())
}

fn identity_error(message: &str) -> SshError {
    SshError::new(SshErrorKind::IdentityFile, message)
}

fn agent_error(technical: impl Into<String>) -> SshError {
    SshError::new(
        SshErrorKind::AgentUnavailable,
        "The SSH Agent is unavailable or has no usable identities.",
    )
    .with_technical(technical)
}

fn timeout_error() -> SshError {
    SshError::new(SshErrorKind::Timeout, "The SSH connection timed out.")
}

fn authentication_rejected_error() -> SshError {
    SshError::new(
        SshErrorKind::AuthenticationRejected,
        "The SSH server rejected authentication.",
    )
}

fn cancelled_error() -> SshError {
    SshError::new(SshErrorKind::Cancelled, "The SSH operation was cancelled.")
}

fn not_implemented(operation: &str) -> SshError {
    SshError::new(
        SshErrorKind::Protocol,
        format!("{operation} will be implemented by the interactive-session task."),
    )
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[derive(Default)]
    struct SuccessfulAuthenticator {
        identity_calls: Vec<(String, PathBuf)>,
        agent_users: Vec<String>,
    }

    #[async_trait]
    impl AuthenticationDriver for SuccessfulAuthenticator {
        async fn identity_file(&mut self, username: &str, path: &Path) -> Result<bool, SshError> {
            self.identity_calls
                .push((username.to_owned(), path.to_path_buf()));
            Ok(true)
        }

        async fn agent(&mut self, username: &str) -> Result<bool, SshError> {
            self.agent_users.push(username.to_owned());
            Ok(true)
        }
    }

    fn host(auth_method: AuthMethod, identity_file: Option<&str>) -> Host {
        Host {
            id: "lab".to_owned(),
            name: "Lab".to_owned(),
            address: "192.0.2.10".to_owned(),
            port: 22,
            username: "operator".to_owned(),
            group: None,
            tags: Vec::new(),
            auth_method,
            identity_file: identity_file.map(PathBuf::from),
        }
    }

    #[test]
    fn identity_path_expansion_is_safe() {
        assert_eq!(
            expand_identity_path_from(Path::new("~/.ssh/id_ed25519"), Path::new("/home/alice"))
                .unwrap(),
            PathBuf::from("/home/alice/.ssh/id_ed25519")
        );
        assert_eq!(
            expand_identity_path_from(Path::new("~root/.ssh/key"), Path::new("/home/alice"))
                .unwrap_err()
                .kind(),
            SshErrorKind::IdentityFile
        );
    }

    #[tokio::test]
    async fn identity_file_and_agent_success_paths_are_both_covered() {
        let identity = connect_request_for_host(
            &host(AuthMethod::IdentityFile, Some("/keys/id_ed25519")),
            Duration::from_secs(7),
        )
        .unwrap();
        assert_eq!(
            identity.authentication,
            Authentication::IdentityFile(PathBuf::from("/keys/id_ed25519"))
        );
        let agent =
            connect_request_for_host(&host(AuthMethod::Agent, None), Duration::from_secs(7))
                .unwrap();
        assert_eq!(agent.authentication, Authentication::Agent);

        let mut driver = SuccessfulAuthenticator::default();
        assert!(
            authenticate(&mut driver, &identity.username, &identity.authentication)
                .await
                .unwrap()
        );
        assert!(
            authenticate(&mut driver, &agent.username, &agent.authentication)
                .await
                .unwrap()
        );
        assert_eq!(
            driver.identity_calls,
            vec![("operator".to_owned(), PathBuf::from("/keys/id_ed25519"))]
        );
        assert_eq!(driver.agent_users, vec!["operator"]);
    }

    #[test]
    fn key_agent_auth_refusal_and_timeout_errors_remain_distinct() {
        let key = identity_error("key");
        let agent = agent_error("agent");
        let auth = authentication_rejected_error();
        let refusal = map_russh_error(russh::Error::IO(std::io::Error::new(
            ErrorKind::ConnectionRefused,
            "refused",
        )));
        let timeout = timeout_error();
        assert_eq!(key.kind(), SshErrorKind::IdentityFile);
        assert_eq!(agent.kind(), SshErrorKind::AgentUnavailable);
        assert_eq!(auth.kind(), SshErrorKind::AuthenticationRejected);
        assert_eq!(refusal.kind(), SshErrorKind::ConnectionRefused);
        assert_eq!(timeout.kind(), SshErrorKind::Timeout);
    }

    #[tokio::test]
    async fn timeout_covers_the_entire_connect_and_authentication_operation() {
        let error = run_with_deadline(
            Duration::from_millis(1),
            CancellationToken::new(),
            std::future::pending::<Result<(), SshError>>(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), SshErrorKind::Timeout);
    }
}
