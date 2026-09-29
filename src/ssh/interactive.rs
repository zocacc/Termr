use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{CancellationToken, PtyRequest, SshConnection, SshError, SshErrorKind};

/// A byte-stream PTY actor for line-oriented shells and network-device CLIs.
///
/// It deliberately provides no terminal emulation, scrollback, transcript, or
/// persistence. Consumers render output bytes directly and keep any display
/// buffer ephemeral.
pub struct InteractiveSession {
    commands: mpsc::UnboundedSender<InteractiveCommand>,
    events: mpsc::Receiver<InteractiveEvent>,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractiveEvent {
    Ready,
    Output(Vec<u8>),
    Closed,
    Error(InteractiveFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveFailure {
    pub kind: SshErrorKind,
    pub message: String,
    pub technical: Option<String>,
}

enum InteractiveCommand {
    Input(Vec<u8>),
    Resize { columns: u32, rows: u32 },
    Close,
}

impl InteractiveSession {
    pub fn spawn(mut connection: Box<dyn SshConnection>, request: PtyRequest) -> Self {
        let (command_sender, mut commands) = mpsc::unbounded_channel();
        let (event_sender, events) = mpsc::channel(64);
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            let mut pty = match connection
                .open_pty(request, worker_cancellation.clone())
                .await
            {
                Ok(pty) => pty,
                Err(error) => {
                    let _ =
                        send_event(&event_sender, &worker_cancellation, error_event(error)).await;
                    let _ = connection.close().await;
                    return;
                }
            };
            if !send_event(&event_sender, &worker_cancellation, InteractiveEvent::Ready).await {
                let _ = pty.close().await;
                let _ = connection.close().await;
                return;
            }

            loop {
                enum Next {
                    Command(Option<InteractiveCommand>),
                    Output(Result<Option<Vec<u8>>, SshError>),
                    Cancelled,
                }
                let next = tokio::select! {
                    command = commands.recv() => Next::Command(command),
                    output = pty.next_output(worker_cancellation.clone()) => Next::Output(output),
                    _ = worker_cancellation.cancelled() => Next::Cancelled,
                };
                let result = match next {
                    Next::Command(Some(InteractiveCommand::Input(input))) => {
                        pty.send_input(&input, worker_cancellation.clone()).await
                    }
                    Next::Command(Some(InteractiveCommand::Resize { columns, rows })) => {
                        pty.resize(columns, rows, worker_cancellation.clone()).await
                    }
                    Next::Command(Some(InteractiveCommand::Close) | None) | Next::Cancelled => {
                        break;
                    }
                    Next::Output(Ok(Some(output))) => {
                        if !send_event(
                            &event_sender,
                            &worker_cancellation,
                            InteractiveEvent::Output(output),
                        )
                        .await
                        {
                            break;
                        }
                        Ok(())
                    }
                    Next::Output(Ok(None)) => break,
                    Next::Output(Err(error)) => Err(error),
                };
                if let Err(error) = result {
                    let _ =
                        send_event(&event_sender, &worker_cancellation, error_event(error)).await;
                    break;
                }
            }

            let _ = pty.close().await;
            let _ = connection.close().await;
            let _ = send_event(
                &event_sender,
                &worker_cancellation,
                InteractiveEvent::Closed,
            )
            .await;
        });
        Self {
            commands: command_sender,
            events,
            cancellation,
            task,
        }
    }

    pub fn send_input(&self, input: impl Into<Vec<u8>>) -> Result<(), SshError> {
        self.commands
            .send(InteractiveCommand::Input(input.into()))
            .map_err(|_| command_closed_error())
    }

    pub fn resize(&self, columns: u32, rows: u32) -> Result<(), SshError> {
        self.commands
            .send(InteractiveCommand::Resize { columns, rows })
            .map_err(|_| command_closed_error())
    }

    pub async fn next_event(&mut self) -> Option<InteractiveEvent> {
        self.events.recv().await
    }

    pub fn close(&self) {
        self.cancellation.cancel();
        let _ = self.commands.send(InteractiveCommand::Close);
    }

    pub async fn finish(self) {
        let _ = self.task.await;
    }
}

fn error_event(error: SshError) -> InteractiveEvent {
    InteractiveEvent::Error(InteractiveFailure {
        kind: error.kind(),
        message: error.to_string(),
        technical: error.technical().map(str::to_owned),
    })
}

async fn send_event(
    sender: &mpsc::Sender<InteractiveEvent>,
    cancellation: &CancellationToken,
    event: InteractiveEvent,
) -> bool {
    tokio::select! {
        _ = cancellation.cancelled() => false,
        result = sender.send(event) => result.is_ok(),
    }
}

fn command_closed_error() -> SshError {
    SshError::new(SshErrorKind::Closed, "The interactive session is closed.")
}

#[cfg(test)]
mod pty {
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::ssh::fake::{FakeScript, FakeSshClient};
    use crate::ssh::{Authentication, ConnectRequest, HostKey, HostKeyVerifier, SshClient};

    struct AcceptKeys;

    #[async_trait]
    impl HostKeyVerifier for AcceptKeys {
        async fn verify(&self, _request: &ConnectRequest, _key: &HostKey) -> Result<(), SshError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn streams_line_cli_input_output_resize_and_closes_without_transcript() {
        let client = FakeSshClient::new(FakeScript {
            pty_output_delay: Duration::from_millis(20),
            pty_output: vec![b"switch> ".to_vec(), b"show version\r\nOK\r\n".to_vec()],
            ..FakeScript::default()
        });
        let connection = client
            .connect(
                ConnectRequest {
                    host_id: "switch".to_owned(),
                    address: "192.0.2.40".to_owned(),
                    port: 22,
                    username: "admin".to_owned(),
                    authentication: Authentication::Agent,
                    timeout: Duration::from_secs(5),
                },
                Arc::new(AcceptKeys),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut session =
            InteractiveSession::spawn(connection, PtyRequest::new("xterm-256color", 80, 24));
        assert_eq!(session.next_event().await, Some(InteractiveEvent::Ready));
        session.send_input(b"show version\n".to_vec()).unwrap();
        session.resize(132, 43).unwrap();
        assert_eq!(
            session.next_event().await,
            Some(InteractiveEvent::Output(b"switch> ".to_vec()))
        );
        assert_eq!(
            session.next_event().await,
            Some(InteractiveEvent::Output(b"show version\r\nOK\r\n".to_vec()))
        );
        while !matches!(
            session.next_event().await,
            Some(InteractiveEvent::Closed) | None
        ) {}
        session.finish().await;

        let observation = client.observation();
        assert_eq!(observation.pty_input, b"show version\n");
        assert_eq!(observation.pty_size, Some((132, 43)));
        assert!(observation.pty_closed);
        assert!(observation.connection_closed);
    }

    #[tokio::test]
    async fn close_cancels_a_worker_blocked_by_output_backpressure() {
        let client = FakeSshClient::new(FakeScript {
            pty_output: (0..100).map(|_| b"output\n".to_vec()).collect(),
            ..FakeScript::default()
        });
        let connection = client
            .connect(
                ConnectRequest {
                    host_id: "switch".to_owned(),
                    address: "192.0.2.40".to_owned(),
                    port: 22,
                    username: "admin".to_owned(),
                    authentication: Authentication::Agent,
                    timeout: Duration::from_secs(5),
                },
                Arc::new(AcceptKeys),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut session =
            InteractiveSession::spawn(connection, PtyRequest::new("xterm-256color", 80, 24));
        assert_eq!(session.next_event().await, Some(InteractiveEvent::Ready));
        tokio::time::sleep(Duration::from_millis(10)).await;
        session.close();
        tokio::time::timeout(Duration::from_millis(200), session.finish())
            .await
            .expect("interactive worker did not stop after cancellation");
    }

    #[tokio::test]
    async fn input_admission_does_not_drop_when_the_worker_is_busy() {
        let client = FakeSshClient::new(FakeScript {
            pty_output_delay: Duration::from_secs(30),
            ..FakeScript::default()
        });
        let connection = client
            .connect(
                ConnectRequest {
                    host_id: "switch".to_owned(),
                    address: "192.0.2.40".to_owned(),
                    port: 22,
                    username: "admin".to_owned(),
                    authentication: Authentication::Agent,
                    timeout: Duration::from_secs(5),
                },
                Arc::new(AcceptKeys),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut session =
            InteractiveSession::spawn(connection, PtyRequest::new("xterm-256color", 80, 24));
        assert_eq!(session.next_event().await, Some(InteractiveEvent::Ready));
        for _ in 0..100 {
            session.send_input(b"x".to_vec()).unwrap();
        }
        session.close();
        session.finish().await;
    }
}
