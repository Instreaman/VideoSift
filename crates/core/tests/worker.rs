use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use videosift_core::worker::{HandshakeError, probe_worker};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn python() -> PathBuf {
    root().join(if cfg!(windows) {
        "worker/.venv/Scripts/python.exe"
    } else {
        "worker/.venv/bin/python"
    })
}

async fn mock(script: &str) -> Result<(), HandshakeError> {
    let directory = tempfile::tempdir().unwrap();
    let entry = directory.path().join("worker.py");
    std::fs::write(&entry, script).unwrap();
    probe_worker(&python(), &entry).await
}

#[tokio::test]
async fn real_worker_handshake() {
    probe_worker(&python(), &root().join("worker/main.py"))
        .await
        .unwrap();
}

#[tokio::test]
async fn incompatible_worker() {
    let script = r#"import sys,json
request=json.loads(sys.stdin.readline())
message="Unsupported version"
print(json.dumps({"jsonrpc":"2.0","id":request["id"],"error":{"code":1009,"message":message,
"data":{"protocol_version":"0.1.0","task_id":None,"message":message,"code":"configuration",
"recoverable":False,"details":{"reason":"protocol_version_mismatch"}}}}),flush=True)
sys.exit(2)
"#;
    assert!(matches!(
        mock(script).await,
        Err(HandshakeError::VersionMismatch)
    ));
}

#[tokio::test]
async fn partial_reply_times_out_and_is_reaped() {
    let start = Instant::now();
    let result =
        mock("import sys,time\nsys.stdin.readline()\nprint('{',end='',flush=True)\ntime.sleep(60)")
            .await;
    assert!(matches!(result, Err(HandshakeError::Timeout)));
    assert!((Duration::from_secs(5)..Duration::from_secs(10)).contains(&start.elapsed()));
}

#[tokio::test]
async fn crash_before_response_is_reaped() {
    assert!(matches!(
        mock("import os,sys\nsys.stdin.readline()\nos._exit(7)").await,
        Err(HandshakeError::UnexpectedExit)
    ));
}
