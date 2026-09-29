use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::{ConnectRequest, HostKey, HostKeyVerifier, SshError, SshErrorKind};

#[async_trait]
pub trait HostKeyPrompt: Send + Sync {
    async fn confirm(&self, request: &ConnectRequest, key: &HostKey) -> Result<bool, SshError>;
}

pub struct HostKeyPromptRequest {
    pub host_id: String,
    pub address: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
    response: oneshot::Sender<bool>,
}

impl HostKeyPromptRequest {
    pub fn respond(self, trusted: bool) {
        let _ = self.response.send(trusted);
    }
}

#[derive(Clone)]
pub struct ChannelHostKeyPrompt {
    sender: mpsc::Sender<HostKeyPromptRequest>,
}

impl ChannelHostKeyPrompt {
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<HostKeyPromptRequest>) {
        let (sender, receiver) = mpsc::channel(capacity);
        (Self { sender }, receiver)
    }
}

#[async_trait]
impl HostKeyPrompt for ChannelHostKeyPrompt {
    async fn confirm(&self, request: &ConnectRequest, key: &HostKey) -> Result<bool, SshError> {
        let (response, answer) = oneshot::channel();
        self.sender
            .send(HostKeyPromptRequest {
                host_id: request.host_id.clone(),
                address: request.address.clone(),
                port: request.port,
                algorithm: key.algorithm.clone(),
                fingerprint: key.fingerprint.clone(),
                response,
            })
            .await
            .map_err(|_| prompt_closed_error())?;
        answer.await.map_err(|_| prompt_closed_error())
    }
}

pub struct KnownHostsVerifier {
    path: PathBuf,
    prompt: Arc<dyn HostKeyPrompt>,
    verification_lock: Mutex<()>,
}

impl KnownHostsVerifier {
    pub fn new(path: impl Into<PathBuf>, prompt: Arc<dyn HostKeyPrompt>) -> Self {
        Self {
            path: path.into(),
            prompt,
            verification_lock: Mutex::new(()),
        }
    }
}

#[async_trait]
impl HostKeyVerifier for KnownHostsVerifier {
    async fn verify(&self, request: &ConnectRequest, key: &HostKey) -> Result<(), SshError> {
        let _guard = self.verification_lock.lock().await;
        let path = self.path.clone();
        let known = tokio::task::spawn_blocking(move || KnownHostsFile::load(&path))
            .await
            .map_err(|error| storage_error("The known-hosts check failed.", error.to_string()))??;

        match known.match_key(request, key) {
            KeyMatch::Known => return Ok(()),
            KeyMatch::Changed => {
                return Err(SshError::new(
                    SshErrorKind::HostKeyChanged,
                    "The SSH host key changed. Connection was blocked.",
                ));
            }
            KeyMatch::Unknown => {}
        }

        if !self.prompt.confirm(request, key).await? {
            return Err(SshError::new(
                SshErrorKind::HostKeyUnknown,
                "The SSH host key was not trusted.",
            ));
        }

        let path = self.path.clone();
        let request = request.clone();
        let key = key.clone();
        tokio::task::spawn_blocking(move || {
            let mut latest = KnownHostsFile::load(&path)?;
            match latest.match_key(&request, &key) {
                KeyMatch::Known => Ok(()),
                KeyMatch::Changed => Err(SshError::new(
                    SshErrorKind::HostKeyChanged,
                    "The SSH host key changed while trust was being confirmed.",
                )),
                KeyMatch::Unknown => {
                    latest.entries.push(KnownHostEntry::from((&request, &key)));
                    latest.save_atomic(&path)
                }
            }
        })
        .await
        .map_err(|error| storage_error("The host-key acceptance failed.", error.to_string()))?
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct KnownHostsFile {
    #[serde(default)]
    entries: Vec<KnownHostEntry>,
}

impl KnownHostsFile {
    fn load(path: &Path) -> Result<Self, SshError> {
        match fs::read_to_string(path) {
            Ok(contents) => serde_yaml::from_str(&contents).map_err(|error| {
                storage_error("Termr's known_hosts file is invalid.", error.to_string())
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(storage_error(
                "Termr's known_hosts file could not be read.",
                error.to_string(),
            )),
        }
    }

    fn match_key(&self, request: &ConnectRequest, key: &HostKey) -> KeyMatch {
        let matching: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| entry.address == request.address && entry.port == request.port)
            .collect();
        if matching.iter().any(|entry| {
            entry.algorithm == key.algorithm && entry.public_key_base64 == key.public_key_base64
        }) {
            KeyMatch::Known
        } else if !matching.is_empty() {
            KeyMatch::Changed
        } else {
            KeyMatch::Unknown
        }
    }

    fn save_atomic(&self, path: &Path) -> Result<(), SshError> {
        let parent = path.parent().ok_or_else(|| {
            storage_error(
                "The known_hosts path has no parent directory.",
                "missing parent",
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            storage_error(
                "The known_hosts directory could not be created.",
                error.to_string(),
            )
        })?;
        set_directory_permissions(parent)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = parent.join(format!(".known_hosts.{}.{nonce}.tmp", std::process::id()));
        let contents = serde_yaml::to_string(self).map_err(|error| {
            storage_error(
                "The known_hosts data could not be encoded.",
                error.to_string(),
            )
        })?;
        let result = write_restrictive(&temporary, contents.as_bytes())
            .and_then(|()| fs::rename(&temporary, path));
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(storage_error(
                "The host key could not be saved atomically.",
                error.to_string(),
            ));
        }
        set_file_permissions(path)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct KnownHostEntry {
    host_id: String,
    address: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
    public_key_base64: String,
}

impl From<(&ConnectRequest, &HostKey)> for KnownHostEntry {
    fn from((request, key): (&ConnectRequest, &HostKey)) -> Self {
        Self {
            host_id: request.host_id.clone(),
            address: request.address.clone(),
            port: request.port,
            algorithm: key.algorithm.clone(),
            fingerprint: key.fingerprint.clone(),
            public_key_base64: key.public_key_base64.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyMatch {
    Known,
    Unknown,
    Changed,
}

fn write_restrictive(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<(), SshError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        storage_error(
            "The known_hosts directory permissions could not be restricted.",
            error.to_string(),
        )
    })
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> Result<(), SshError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<(), SshError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        storage_error(
            "The known_hosts permissions could not be restricted.",
            error.to_string(),
        )
    })
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> Result<(), SshError> {
    Ok(())
}

fn storage_error(message: &str, technical: impl Into<String>) -> SshError {
    SshError::new(SshErrorKind::Protocol, message).with_technical(technical)
}

fn prompt_closed_error() -> SshError {
    SshError::new(
        SshErrorKind::Closed,
        "The host-key confirmation was closed before a decision was made.",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::tempdir;

    use super::*;
    use crate::ssh::Authentication;

    struct Prompt {
        accept: bool,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl HostKeyPrompt for Prompt {
        async fn confirm(
            &self,
            _request: &ConnectRequest,
            key: &HostKey,
        ) -> Result<bool, SshError> {
            assert_eq!(key.algorithm, "ssh-ed25519");
            assert!(key.fingerprint.starts_with("SHA256:"));
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.accept)
        }
    }

    fn request() -> ConnectRequest {
        ConnectRequest {
            host_id: "lab".to_owned(),
            address: "192.0.2.20".to_owned(),
            port: 22,
            username: "admin".to_owned(),
            authentication: Authentication::Agent,
            timeout: std::time::Duration::from_secs(10),
        }
    }

    fn key(value: &str) -> HostKey {
        HostKey {
            algorithm: "ssh-ed25519".to_owned(),
            fingerprint: format!("SHA256:{value}"),
            public_key_base64: value.to_owned(),
        }
    }

    #[tokio::test]
    async fn unknown_acceptance_is_atomic_and_then_silent() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config/known_hosts");
        let prompt = Arc::new(Prompt {
            accept: true,
            calls: AtomicUsize::new(0),
        });
        let verifier = KnownHostsVerifier::new(path.clone(), prompt.clone());
        verifier.verify(&request(), &key("first")).await.unwrap();
        assert_eq!(prompt.calls.load(Ordering::SeqCst), 1);
        assert!(path.exists());
        verifier.verify(&request(), &key("first")).await.unwrap();
        assert_eq!(prompt.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rejection_persists_nothing() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        let verifier = KnownHostsVerifier::new(
            path.clone(),
            Arc::new(Prompt {
                accept: false,
                calls: AtomicUsize::new(0),
            }),
        );
        let error = verifier
            .verify(&request(), &key("first"))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), SshErrorKind::HostKeyUnknown);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn changed_key_is_blocked_and_never_overwritten() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        let prompt = Arc::new(Prompt {
            accept: true,
            calls: AtomicUsize::new(0),
        });
        let verifier = KnownHostsVerifier::new(path.clone(), prompt.clone());
        verifier.verify(&request(), &key("first")).await.unwrap();
        let before = fs::read_to_string(&path).unwrap();
        let error = verifier
            .verify(&request(), &key("changed"))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), SshErrorKind::HostKeyChanged);
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert_eq!(prompt.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn changed_algorithm_is_also_blocked() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        let prompt = Arc::new(Prompt {
            accept: true,
            calls: AtomicUsize::new(0),
        });
        let verifier = KnownHostsVerifier::new(path, prompt.clone());
        verifier.verify(&request(), &key("first")).await.unwrap();
        let mut changed = key("second");
        changed.algorithm = "rsa-sha2-512".to_owned();
        let error = verifier.verify(&request(), &changed).await.unwrap_err();
        assert_eq!(error.kind(), SshErrorKind::HostKeyChanged);
        assert_eq!(prompt.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn channel_prompt_exposes_algorithm_and_fingerprint() {
        let (prompt, mut requests) = ChannelHostKeyPrompt::new(1);
        let request = request();
        let key = key("visible");
        let task = tokio::spawn(async move { prompt.confirm(&request, &key).await });
        let confirmation = requests.recv().await.unwrap();
        assert_eq!(confirmation.algorithm, "ssh-ed25519");
        assert_eq!(confirmation.fingerprint, "SHA256:visible");
        confirmation.respond(true);
        assert!(task.await.unwrap().unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_permissions_are_restrictive() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().unwrap();
        let config = directory.path().join("config");
        let path = config.join("known_hosts");
        let verifier = KnownHostsVerifier::new(
            path.clone(),
            Arc::new(Prompt {
                accept: true,
                calls: AtomicUsize::new(0),
            }),
        );
        verifier.verify(&request(), &key("first")).await.unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
