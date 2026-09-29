use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::{
    CancellationToken, CommandOutput, ConnectRequest, HostKey, HostKeyVerifier, PtyRequest,
    SshClient, SshConnection, SshError, SshErrorKind, SshPty,
};

#[derive(Debug, Clone)]
pub struct FakeScript {
    pub connect_delay: Duration,
    pub connect_error: Option<SshError>,
    pub exec_delay: Duration,
    pub exec_error: Option<SshError>,
    pub exec_stdout: Vec<u8>,
    pub exec_stderr: Vec<u8>,
    pub exec_status: Option<u32>,
    pub pty_output: Vec<Vec<u8>>,
    pub server_key: HostKey,
}

impl Default for FakeScript {
    fn default() -> Self {
        Self {
            connect_delay: Duration::ZERO,
            connect_error: None,
            exec_delay: Duration::ZERO,
            exec_error: None,
            exec_stdout: Vec::new(),
            exec_stderr: Vec::new(),
            exec_status: Some(0),
            pty_output: Vec::new(),
            server_key: HostKey {
                algorithm: "ssh-ed25519".to_owned(),
                fingerprint: "SHA256:termr-fake-key".to_owned(),
                public_key_base64: "ZmFrZS1wdWJsaWMta2V5".to_owned(),
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FakeObservation {
    pub connections: usize,
    pub commands: Vec<String>,
    pub pty_input: Vec<u8>,
    pub pty_size: Option<(u32, u32)>,
    pub closed: bool,
}

#[derive(Debug, Clone)]
pub struct FakeSshClient {
    script: FakeScript,
    observation: Arc<Mutex<FakeObservation>>,
}

impl FakeSshClient {
    pub fn new(script: FakeScript) -> Self {
        Self {
            script,
            observation: Arc::new(Mutex::new(FakeObservation::default())),
        }
    }

    pub fn observation(&self) -> FakeObservation {
        self.observation.lock().unwrap().clone()
    }
}

#[async_trait]
impl SshClient for FakeSshClient {
    async fn connect(
        &self,
        request: ConnectRequest,
        verifier: Arc<dyn HostKeyVerifier>,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn SshConnection>, SshError> {
        wait_or_cancel(self.script.connect_delay, &cancellation).await?;
        verifier.verify(&request, &self.script.server_key).await?;
        if let Some(error) = &self.script.connect_error {
            return Err(error.clone());
        }

        self.observation.lock().unwrap().connections += 1;
        Ok(Box::new(FakeConnection {
            script: self.script.clone(),
            observation: Arc::clone(&self.observation),
        }))
    }
}

struct FakeConnection {
    script: FakeScript,
    observation: Arc<Mutex<FakeObservation>>,
}

#[async_trait]
impl SshConnection for FakeConnection {
    async fn exec(
        &mut self,
        command: &str,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput, SshError> {
        wait_or_cancel(self.script.exec_delay, &cancellation).await?;
        if let Some(error) = &self.script.exec_error {
            return Err(error.clone());
        }
        self.observation
            .lock()
            .unwrap()
            .commands
            .push(command.to_owned());
        Ok(CommandOutput {
            stdout: self.script.exec_stdout.clone(),
            stderr: self.script.exec_stderr.clone(),
            exit_status: self.script.exec_status,
        })
    }

    async fn open_pty(
        &mut self,
        request: PtyRequest,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn SshPty>, SshError> {
        cancelled(&cancellation)?;
        self.observation.lock().unwrap().pty_size = Some((request.columns, request.rows));
        Ok(Box::new(FakePty {
            output: self.script.pty_output.clone().into(),
            observation: Arc::clone(&self.observation),
        }))
    }

    async fn close(self: Box<Self>) -> Result<(), SshError> {
        self.observation.lock().unwrap().closed = true;
        Ok(())
    }
}

struct FakePty {
    output: VecDeque<Vec<u8>>,
    observation: Arc<Mutex<FakeObservation>>,
}

#[async_trait]
impl SshPty for FakePty {
    async fn send_input(
        &mut self,
        input: &[u8],
        cancellation: CancellationToken,
    ) -> Result<(), SshError> {
        cancelled(&cancellation)?;
        self.observation
            .lock()
            .unwrap()
            .pty_input
            .extend_from_slice(input);
        Ok(())
    }

    async fn resize(
        &mut self,
        columns: u32,
        rows: u32,
        cancellation: CancellationToken,
    ) -> Result<(), SshError> {
        cancelled(&cancellation)?;
        self.observation.lock().unwrap().pty_size = Some((columns, rows));
        Ok(())
    }

    async fn next_output(
        &mut self,
        cancellation: CancellationToken,
    ) -> Result<Option<Vec<u8>>, SshError> {
        cancelled(&cancellation)?;
        Ok(self.output.pop_front())
    }

    async fn close(self: Box<Self>) -> Result<(), SshError> {
        self.observation.lock().unwrap().closed = true;
        Ok(())
    }
}

async fn wait_or_cancel(
    duration: Duration,
    cancellation: &CancellationToken,
) -> Result<(), SshError> {
    tokio::select! {
        _ = tokio::time::sleep(duration) => Ok(()),
        _ = cancellation.cancelled() => Err(cancelled_error()),
    }
}

fn cancelled(cancellation: &CancellationToken) -> Result<(), SshError> {
    if cancellation.is_cancelled() {
        Err(cancelled_error())
    } else {
        Ok(())
    }
}

fn cancelled_error() -> SshError {
    SshError::new(SshErrorKind::Cancelled, "operation cancelled")
}
