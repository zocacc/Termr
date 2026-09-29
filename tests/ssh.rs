use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use termr::ssh::fake::{FakeScript, FakeSshClient};
use termr::ssh::{
    Authentication, CancellationToken, ConnectRequest, HostKey, HostKeyVerifier, PtyRequest,
    SshClient, SshError, SshErrorKind,
};

#[derive(Default)]
struct RecordingVerifier {
    keys: Mutex<Vec<HostKey>>,
}

#[async_trait]
impl HostKeyVerifier for RecordingVerifier {
    async fn verify(&self, _request: &ConnectRequest, key: &HostKey) -> Result<(), SshError> {
        self.keys.lock().unwrap().push(key.clone());
        Ok(())
    }
}

fn request() -> ConnectRequest {
    ConnectRequest {
        host_id: "olt-lab".to_owned(),
        address: "192.0.2.10".to_owned(),
        port: 22,
        username: "admin".to_owned(),
        authentication: Authentication::Agent,
        timeout: Duration::from_secs(10),
    }
}

#[tokio::test]
async fn fake_connects_only_after_host_key_verification() {
    let verifier = Arc::new(RecordingVerifier::default());
    let client = FakeSshClient::new(FakeScript::default());

    let _connection = client
        .connect(request(), verifier.clone(), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(verifier.keys.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn fake_models_distinct_connection_failures() {
    let error = SshError::new(SshErrorKind::ConnectionRefused, "connection refused");
    let client = FakeSshClient::new(FakeScript {
        connect_error: Some(error),
        ..FakeScript::default()
    });

    let result = client
        .connect(
            request(),
            Arc::new(RecordingVerifier::default()),
            CancellationToken::new(),
        )
        .await;
    let error = match result {
        Ok(_) => panic!("connection unexpectedly succeeded"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), SshErrorKind::ConnectionRefused);
    assert_eq!(error.to_string(), "connection refused");
}

#[tokio::test]
async fn cancellation_interrupts_a_delayed_connection() {
    let client = FakeSshClient::new(FakeScript {
        connect_delay: Duration::from_secs(30),
        ..FakeScript::default()
    });
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let result = client
        .connect(
            request(),
            Arc::new(RecordingVerifier::default()),
            cancellation,
        )
        .await;
    let error = match result {
        Ok(_) => panic!("connection unexpectedly succeeded"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), SshErrorKind::Cancelled);
}

#[tokio::test]
async fn fake_exec_and_pty_are_scriptable() {
    let client = FakeSshClient::new(FakeScript {
        exec_stdout: b"Linux test 6.0\n".to_vec(),
        pty_output: vec![b"prompt> ".to_vec(), b"show version\r\nOK\r\n".to_vec()],
        ..FakeScript::default()
    });
    let mut connection = client
        .connect(
            request(),
            Arc::new(RecordingVerifier::default()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let output = connection
        .exec("uname -a", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.stdout, b"Linux test 6.0\n");
    assert_eq!(output.exit_status, Some(0));

    let mut pty = connection
        .open_pty(
            PtyRequest::new("xterm-256color", 80, 24),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        pty.next_output(CancellationToken::new()).await.unwrap(),
        Some(b"prompt> ".to_vec())
    );
    pty.send_input(b"show version\n", CancellationToken::new())
        .await
        .unwrap();
    pty.resize(120, 40, CancellationToken::new()).await.unwrap();
    let observation = client.observation();
    assert_eq!(observation.pty_input, b"show version\n");
    assert_eq!(observation.pty_size, Some((120, 40)));
    assert_eq!(
        pty.next_output(CancellationToken::new()).await.unwrap(),
        Some(b"show version\r\nOK\r\n".to_vec())
    );
    assert_eq!(
        pty.next_output(CancellationToken::new()).await.unwrap(),
        None
    );
}
