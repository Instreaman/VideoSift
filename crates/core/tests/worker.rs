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

#[tokio::test]
async fn supervised_mock_transcription() {
    let transcript = videosift_core::worker::mock_transcribe(
        &python(),
        &root().join("worker/main.py"),
        "synthetic_task",
        "zh-CN",
    )
    .await
    .unwrap();
    assert_eq!(transcript["segments"][0]["origin"], "asr");
    assert_eq!(transcript["task_id"], "synthetic_task");
}

#[tokio::test]
async fn transcription_errors_are_correlated() {
    let directory = tempfile::tempdir().unwrap();
    let entry = directory.path().join("mock.py");
    let setup = r#"import sys,json
sys.stdin.readline()
print(json.dumps(dict(jsonrpc="2.0",id="hello",result=dict(protocol_version="0.1.0",task_id=None,server_name="worker",capabilities=["inference.transcribe"]))),flush=True)
req=json.loads(sys.stdin.readline())
data=dict(protocol_version="0.1.0",task_id=req["params"]["task_id"],code="source",message="Unavailable",recoverable=False)
error=dict(code=1003,message="Unavailable",data=data)
"#;
    for id in ["transcribe", "stale"] {
        std::fs::write(
            &entry,
            format!(
                "{setup}print(json.dumps(dict(jsonrpc='2.0',id='{id}',error=error)),flush=True)"
            ),
        )
        .unwrap();
        let result = videosift_core::worker::mock_transcribe(&python(), &entry, "test", "en").await;
        if id == "stale" {
            assert!(matches!(result, Err(HandshakeError::InvalidResponse)));
        } else {
            assert!(matches!(result, Err(HandshakeError::UnknownAudio)));
        }
    }
}

#[tokio::test]
async fn transcribe_negotiates_capabilities_and_validates_real_payloads() {
    use serde_json::{Value, json};
    let directory = tempfile::tempdir().unwrap();
    let entry = directory.path().join("peer.py");
    let fixtures: Value =
        serde_json::from_str(include_str!("../../../contracts/examples/valid.json")).unwrap();
    let response = &fixtures
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "transcribe_response")
        .unwrap()["instance"]["result"];
    let caps = json!(["test.extra", "inference.transcribe"]);
    let success = json!({"id":"transcribe","jsonrpc":"2.0","result":response});
    let mut empty = success.clone();
    empty["result"]["segments"] = json!([]);
    let mut bad_time = success.clone();
    bad_time["result"]["segments"][0]["end_ms"] = json!(0);
    let mut wrong_task = success.clone();
    wrong_task["result"]["task_id"] = json!("other");
    let mut wrong_language = success.clone();
    wrong_language["result"]["language"] = json!("en");
    let invalid = json!({"id":"transcribe","jsonrpc":"2.0","error":{"code":-32602,"message":"Invalid params"}});
    for (abilities, reply, expected) in [
        (&caps, &success, "ok"),
        (&caps, &empty, "ok"),
        (&caps, &wrong_task, "invalid"),
        (&caps, &wrong_language, "invalid"),
        (&caps, &bad_time, "invalid"),
        (&caps, &invalid, "params"),
        (&json!(["test.extra"]), &success, "missing"),
    ] {
        let config = directory.path().join("frames.json");
        let marker = directory.path().join("received");
        if marker.exists() {
            std::fs::remove_file(&marker).unwrap();
        }
        let hello = json!({"id":"hello","jsonrpc":"2.0","result":{"protocol_version":"0.1.0","task_id":null,"server_name":"worker","capabilities":abilities}});
        std::fs::write(config, json!([hello, reply]).to_string()).unwrap();
        std::fs::write(
            &entry,
            r#"import json,sys,pathlib
root=pathlib.Path(__file__).parent
hello,reply=json.loads((root/"frames.json").read_text(encoding="utf8"))
sys.stdin.readline()
print(json.dumps(hello),flush=True)
if sys.stdin.readline():
    (root/"received").touch()
    print(json.dumps(reply),flush=True)
"#,
        )
        .unwrap();
        let result =
            videosift_core::worker::mock_transcribe(&python(), &entry, "task_demo", "zh-CN").await;
        match expected {
            "ok" => assert_eq!(result.unwrap(), reply["result"]),
            "params" => assert!(matches!(result, Err(HandshakeError::InvalidParams))),
            _ => assert!(matches!(result, Err(HandshakeError::InvalidResponse))),
        }
        assert_eq!(marker.exists(), expected != "missing");
    }
}
