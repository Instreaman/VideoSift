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
const REPLY_AND_NOTICE: &str = r#"import json,sys,time
req=json.loads(sys.stdin.readline())
reply={"jsonrpc":"2.0","id":req["id"],"result":{"server_name":"worker","protocol_version":"0.1.0","task_id":None,"capabilities":[]}}
notice={"jsonrpc":"2.0","method":"process.exiting","params":{"protocol_version":"0.1.0","task_id":None,"exit_code":0}}
first=json.dumps(reply).encode()+b'\n'
second=json.dumps(notice).encode()+b'\n'
out=sys.stdout.buffer
"#;

#[tokio::test]
async fn combined_and_split_frames_have_identical_results() {
    for writes in [
        "out.write(first+second);out.flush()",
        "out.write(first[:15]);out.flush();time.sleep(.05);out.write(first[15:]+second[:25]);out.flush();time.sleep(.05);out.write(second[25:]);out.flush()",
    ] {
        mock(&format!("{REPLY_AND_NOTICE}{writes}")).await.unwrap();
    }
}

#[tokio::test]
async fn large_logs_after_input_eof_are_drained() {
    mock(&format!("{REPLY_AND_NOTICE}out.write(first);out.flush();assert not sys.stdin.read();sys.stderr.buffer.write(b'x'*1048576);sys.stderr.flush();out.write(second);out.flush()")).await.unwrap();
}

#[tokio::test]
async fn rejects_invalid_notification_and_unterminated_tail() {
    for tail in [
        "first",
        "second[:-1]",
        "b'{{}}\\n'",
        "second.replace(b'null',b'123')",
    ] {
        let result = mock(&format!(
            "{REPLY_AND_NOTICE}out.write(first+{tail});out.flush()"
        ))
        .await;
        assert!(matches!(result, Err(HandshakeError::InvalidResponse)));
    }
}
