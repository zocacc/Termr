use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::ssh::{
    CancellationToken, ConnectRequest, HostKeyVerifier, SshClient, SshConnection, SshError,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SessionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Error(SessionFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFailure {
    pub message: String,
    pub technical: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionUpdate {
    pub host_id: String,
    pub state: SessionState,
}

struct SessionEntry {
    state: SessionState,
    attempt: u64,
    cancellation: Option<CancellationToken>,
    connection: Option<Box<dyn SshConnection>>,
}

impl Default for SessionEntry {
    fn default() -> Self {
        Self {
            state: SessionState::Disconnected,
            attempt: 0,
            cancellation: None,
            connection: None,
        }
    }
}

struct ConnectionEvent {
    host_id: String,
    attempt: u64,
    result: Result<Box<dyn SshConnection>, SshError>,
}

pub struct SessionManager {
    client: Arc<dyn SshClient>,
    verifier: Arc<dyn HostKeyVerifier>,
    sessions: HashMap<String, SessionEntry>,
    next_attempt: u64,
    sender: mpsc::UnboundedSender<ConnectionEvent>,
    receiver: mpsc::UnboundedReceiver<ConnectionEvent>,
    connection_tasks: JoinSet<()>,
    close_tasks: JoinSet<()>,
    shutting_down: bool,
}

impl SessionManager {
    pub fn new(client: Arc<dyn SshClient>, verifier: Arc<dyn HostKeyVerifier>) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            client,
            verifier,
            sessions: HashMap::new(),
            next_attempt: 0,
            sender,
            receiver,
            connection_tasks: JoinSet::new(),
            close_tasks: JoinSet::new(),
            shutting_down: false,
        }
    }

    pub fn state(&self, host_id: &str) -> SessionState {
        self.sessions
            .get(host_id)
            .map(|entry| entry.state.clone())
            .unwrap_or_default()
    }

    pub fn connect(&mut self, request: ConnectRequest) -> bool {
        self.reap_completed_tasks();
        if self.shutting_down {
            return false;
        }
        let entry = self.sessions.entry(request.host_id.clone()).or_default();
        if matches!(
            entry.state,
            SessionState::Connecting | SessionState::Connected
        ) {
            return false;
        }

        self.next_attempt = self.next_attempt.wrapping_add(1).max(1);
        let attempt = self.next_attempt;
        let cancellation = CancellationToken::new();
        entry.state = SessionState::Connecting;
        entry.attempt = attempt;
        entry.cancellation = Some(cancellation.clone());
        entry.connection = None;

        let client = Arc::clone(&self.client);
        let verifier = Arc::clone(&self.verifier);
        let sender = self.sender.clone();
        let host_id = request.host_id.clone();
        self.connection_tasks.spawn(async move {
            let result = client.connect(request, verifier, cancellation).await;
            let _ = sender.send(ConnectionEvent {
                host_id,
                attempt,
                result,
            });
        });
        true
    }

    pub async fn next_update(&mut self) -> Option<SessionUpdate> {
        self.reap_completed_tasks();
        while let Some(event) = self.receiver.recv().await {
            if let Some(update) = self.apply_event(event) {
                return Some(update);
            }
        }
        None
    }

    pub fn try_next_update(&mut self) -> Option<SessionUpdate> {
        self.reap_completed_tasks();
        while let Ok(event) = self.receiver.try_recv() {
            if let Some(update) = self.apply_event(event) {
                return Some(update);
            }
        }
        None
    }

    pub fn disconnect(&mut self, host_id: &str) -> bool {
        self.reap_completed_tasks();
        let Some(entry) = self.sessions.get_mut(host_id) else {
            return false;
        };
        entry.attempt = entry.attempt.wrapping_add(1);
        if let Some(cancellation) = entry.cancellation.take() {
            cancellation.cancel();
        }
        let connection = entry.connection.take();
        let changed = entry.state != SessionState::Disconnected;
        entry.state = SessionState::Disconnected;
        if let Some(connection) = connection {
            self.close_connection(connection);
        }
        changed
    }

    pub fn remove_host(&mut self, host_id: &str) {
        self.disconnect(host_id);
        self.sessions.remove(host_id);
    }

    pub fn shutdown(&mut self) {
        self.shutting_down = true;
        let host_ids: Vec<_> = self.sessions.keys().cloned().collect();
        for host_id in host_ids {
            self.disconnect(&host_id);
        }
    }

    pub async fn finish_shutdown(&mut self) {
        while self.connection_tasks.join_next().await.is_some() {}
        while let Ok(event) = self.receiver.try_recv() {
            self.apply_event(event);
        }
        while self.close_tasks.join_next().await.is_some() {}
    }

    fn apply_event(&mut self, event: ConnectionEvent) -> Option<SessionUpdate> {
        let current = self.sessions.get(&event.host_id).is_some_and(|entry| {
            entry.attempt == event.attempt && entry.state == SessionState::Connecting
        });
        if !current {
            if let Ok(connection) = event.result {
                self.close_connection(connection);
            }
            return None;
        }

        let entry = self
            .sessions
            .get_mut(&event.host_id)
            .expect("checked above");
        entry.cancellation = None;
        entry.state = match event.result {
            Ok(connection) => {
                entry.connection = Some(connection);
                SessionState::Connected
            }
            Err(error) => SessionState::Error(SessionFailure {
                message: error.to_string(),
                technical: error.technical().map(str::to_owned),
            }),
        };
        Some(SessionUpdate {
            host_id: event.host_id,
            state: entry.state.clone(),
        })
    }

    fn close_connection(&mut self, connection: Box<dyn SshConnection>) {
        self.close_tasks.spawn(async move {
            let _ = connection.close().await;
        });
    }

    fn reap_completed_tasks(&mut self) {
        while self.connection_tasks.try_join_next().is_some() {}
        while self.close_tasks.try_join_next().is_some() {}
    }
}

#[cfg(test)]
mod session_state {
    use async_trait::async_trait;

    use super::*;
    use crate::ssh::fake::{FakeScript, FakeSshClient};
    use crate::ssh::{Authentication, HostKey};

    struct AcceptKeys;

    #[async_trait]
    impl HostKeyVerifier for AcceptKeys {
        async fn verify(&self, _request: &ConnectRequest, _key: &HostKey) -> Result<(), SshError> {
            Ok(())
        }
    }

    fn request() -> ConnectRequest {
        ConnectRequest {
            host_id: "lab".to_owned(),
            address: "192.0.2.30".to_owned(),
            port: 22,
            username: "admin".to_owned(),
            authentication: Authentication::Agent,
            timeout: std::time::Duration::from_secs(10),
        }
    }

    fn manager(script: FakeScript) -> SessionManager {
        SessionManager::new(Arc::new(FakeSshClient::new(script)), Arc::new(AcceptKeys))
    }

    #[tokio::test]
    async fn duplicate_attempts_are_prevented_and_success_connects() {
        let mut manager = manager(FakeScript::default());
        assert!(manager.connect(request()));
        assert!(!manager.connect(request()));
        assert_eq!(manager.state("lab"), SessionState::Connecting);
        let update = manager.next_update().await.unwrap();
        assert_eq!(update.state, SessionState::Connected);
        assert_eq!(manager.state("lab"), SessionState::Connected);
    }

    #[tokio::test]
    async fn errors_keep_safe_and_technical_context_separate_and_can_retry() {
        let mut manager = manager(FakeScript {
            connect_error: Some(
                SshError::new(crate::ssh::SshErrorKind::Timeout, "Connection timed out")
                    .with_technical("socket elapsed at 192.0.2.30:22"),
            ),
            ..FakeScript::default()
        });
        assert!(manager.connect(request()));
        let update = manager.next_update().await.unwrap();
        assert_eq!(
            update.state,
            SessionState::Error(SessionFailure {
                message: "Connection timed out".to_owned(),
                technical: Some("socket elapsed at 192.0.2.30:22".to_owned()),
            })
        );
        assert!(manager.connect(request()));
    }

    #[tokio::test]
    async fn stale_events_cannot_replace_a_newer_state() {
        let mut manager = manager(FakeScript::default());
        assert!(manager.connect(request()));
        let old_attempt = manager.sessions["lab"].attempt;
        assert!(manager.disconnect("lab"));
        assert!(manager.connect(request()));
        manager.apply_event(ConnectionEvent {
            host_id: "lab".to_owned(),
            attempt: old_attempt,
            result: Err(SshError::new(
                crate::ssh::SshErrorKind::Protocol,
                "stale failure",
            )),
        });
        assert_eq!(manager.state("lab"), SessionState::Connecting);
    }

    #[tokio::test]
    async fn disconnect_removal_and_shutdown_are_non_blocking() {
        let mut manager = manager(FakeScript {
            connect_delay: std::time::Duration::from_secs(30),
            ..FakeScript::default()
        });
        assert!(manager.connect(request()));
        assert!(manager.disconnect("lab"));
        assert_eq!(manager.state("lab"), SessionState::Disconnected);
        assert!(manager.connect(request()));
        manager.remove_host("lab");
        assert_eq!(manager.state("lab"), SessionState::Disconnected);
        assert!(manager.connect(request()));
        manager.shutdown();
        assert_eq!(manager.state("lab"), SessionState::Disconnected);
        assert!(!manager.connect(request()));
    }

    #[tokio::test]
    async fn exit_drain_closes_established_connections() {
        let client = FakeSshClient::new(FakeScript::default());
        let mut manager = SessionManager::new(Arc::new(client.clone()), Arc::new(AcceptKeys));
        assert!(manager.connect(request()));
        assert_eq!(
            manager.next_update().await.unwrap().state,
            SessionState::Connected
        );

        manager.shutdown();
        assert_eq!(manager.state("lab"), SessionState::Disconnected);
        manager.finish_shutdown().await;

        assert!(client.observation().connection_closed);
    }
}
