use std::collections::HashSet;
use std::io::{BufRead, Cursor};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::{Instant, timeout_at};

use crate::process::{Limits, ProcessError, ProcessSpec, ProcessSupervisor, Stream, Termination};
use crate::scheduler::Execution;

#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error("worker session cancelled")]
    Cancelled,
    #[error("worker response timed out")]
    Timeout,
    #[error("incompatible worker protocol")]
    VersionMismatch,
    #[error("invalid worker handshake response")]
    InvalidResponse,
    #[error("invalid transcription parameters")]
    InvalidParams,
    #[error("unknown mock audio reference")]
    UnknownAudio,
    #[error("worker exited unexpectedly")]
    UnexpectedExit,
}

/// Probe a trusted isolated Python environment and Worker entry point, then reap the process.
pub async fn probe_worker(python: &Path, entry: &Path) -> Result<(), HandshakeError> {
    session(python, entry, None, None).await.map(|_| ())
}

/// Run one synthetic transcription against the virtual audio/mock.bin artifact.
pub async fn mock_transcribe(
    python: &Path,
    entry: &Path,
    task_id: &str,
    language: &str,
) -> Result<Value, HandshakeError> {
    mock_transcribe_inner(python, entry, task_id, language, None).await
}

/// Request cancellation by publishing true and keeping it set until this call returns.
/// A pre-existing request prevents dispatch. Dropping the sender alone does not cancel.
pub async fn mock_transcribe_cancellable(
    python: &Path,
    entry: &Path,
    task_id: &str,
    language: &str,
    cancellation: watch::Receiver<bool>,
) -> Result<Value, HandshakeError> {
    mock_transcribe_inner(python, entry, task_id, language, Some(cancellation)).await
}

async fn mock_transcribe_inner(
    python: &Path,
    entry: &Path,
    task_id: &str,
    language: &str,
    cancellation: Option<watch::Receiver<bool>>,
) -> Result<Value, HandshakeError> {
    if task_id.is_empty()
        || task_id.len() > 128
        || !task_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        || !(2..=35).contains(&language.chars().count())
    {
        return Err(HandshakeError::InvalidParams);
    }
    session(python, entry, Some((task_id, language)), cancellation)
        .await?
        .ok_or(HandshakeError::InvalidResponse)
}

async fn session(
    python: &Path,
    entry: &Path,
    transcription: Option<(&str, &str)>,
    mut cancellation: Option<watch::Receiver<bool>>,
) -> Result<Option<Value>, HandshakeError> {
    if !entry.is_absolute() {
        return Err(ProcessError::InvalidProgram.into());
    }
    if cancellation.as_ref().is_some_and(|signal| *signal.borrow()) {
        return Err(HandshakeError::Cancelled);
    }
    let mut supervisor = ProcessSupervisor::new(1, 1)?;
    let task = supervisor.submit()?;
    let mut spec = ProcessSpec {
        program: python.into(),
        args: vec!["-I".into(), entry.into(), "--supervised".into()],
    };
    if transcription.is_some() {
        spec.args.push("--mock-transcribe".into());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let execution = supervisor
        .dispatch(
            spec,
            Limits {
                pipe_chunks: 4,
                run_timeout: Duration::from_secs(10),
                cancel_grace: Duration::from_millis(200),
                cleanup_timeout: Duration::from_secs(5),
            },
        )?
        .ok_or(ProcessError::NotManaged)?;
    let result = tokio::select! {
        biased;
        () = cancelled(&mut cancellation) => Err(HandshakeError::Cancelled),
        result = exchange_and_drain(&mut supervisor, execution, deadline, transcription) => result,
    };
    let (report, transcript) = match result {
        Err(error) => {
            supervisor.cancel(task)?;
            supervisor.settle(execution).await?;
            return Err(error);
        }
        Ok(transcript) => {
            let report = tokio::select! {
                biased;
                () = cancelled(&mut cancellation) => {
                    supervisor.cancel(task)?;
                    supervisor.settle(execution).await?;
                    return Err(HandshakeError::Cancelled);
                }
                report = supervisor.settle(execution) => report?,
            };
            (report, transcript)
        }
    };
    if report.pipe_failed || report.termination != (Termination::Exited { success: true }) {
        return Err(HandshakeError::UnexpectedExit);
    }
    Ok(transcript)
}

async fn cancelled(signal: &mut Option<watch::Receiver<bool>>) {
    if let Some(signal) = signal
        && signal.wait_for(|requested| *requested).await.is_ok()
    {
        return;
    }
    std::future::pending().await
}

async fn exchange_and_drain(
    supervisor: &mut ProcessSupervisor,
    execution: Execution,
    deadline: Instant,
    transcription: Option<(&str, &str)>,
) -> Result<Option<Value>, HandshakeError> {
    let mut frames = Frames::default();
    let mut result = match timeout_at(
        deadline,
        exchange(supervisor, execution, &mut frames, transcription.is_some()),
    )
    .await
    {
        Ok(result) if Instant::now() < deadline => result,
        _ => Err(HandshakeError::Timeout),
    };
    let mut transcript = None;
    if result.is_ok()
        && let Some((task_id, language)) = transcription
    {
        let response_deadline = Instant::now() + Duration::from_secs(3);
        result = match timeout_at(
            response_deadline,
            transcribe(supervisor, execution, &mut frames, task_id, language),
        )
        .await
        {
            Ok(reply) if Instant::now() < response_deadline => {
                reply.map(|value| transcript = Some(value))
            }
            _ => Err(HandshakeError::Timeout),
        };
    }
    if result.is_ok() {
        supervisor.close_input(execution)?;
        result = frames.drain(supervisor, execution).await;
    }
    result.map(|()| transcript)
}

#[derive(Default)]
struct Frames {
    chunk: Cursor<Vec<u8>>,
    partial: Vec<u8>,
}

impl Frames {
    async fn next(
        &mut self,
        supervisor: &mut ProcessSupervisor,
        execution: Execution,
    ) -> Result<Option<Value>, HandshakeError> {
        loop {
            let bytes = self.chunk.fill_buf().expect("in-memory cursor");
            if bytes.is_empty() {
                let Some(chunk) = supervisor.recv_output(execution).await? else {
                    return if self.partial.is_empty() {
                        Ok(None)
                    } else {
                        Err(HandshakeError::InvalidResponse)
                    };
                };
                if chunk.stream == Stream::Stdout {
                    self.chunk = Cursor::new(chunk.bytes);
                }
                continue;
            }
            let lf = bytes.iter().position(|b| *b == b'\n');
            let take = lf.map_or(bytes.len(), |offset| offset + 1);
            if self.partial.len() + take > 1_048_576 {
                return Err(HandshakeError::InvalidResponse);
            }
            self.partial.extend_from_slice(&bytes[..take]);
            self.chunk.consume(take);
            if lf.is_some() {
                let frame = serde_json::from_slice(&self.partial)
                    .map_err(|_| HandshakeError::InvalidResponse)?;
                self.partial.clear();
                return Ok(Some(frame));
            }
        }
    }

    async fn drain(
        &mut self,
        supervisor: &mut ProcessSupervisor,
        execution: Execution,
    ) -> Result<(), HandshakeError> {
        while let Some(frame) = self.next(supervisor, execution).await? {
            // A handshake-only connection owns no inference task.
            let code = frame
                .pointer("/params/exit_code")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or(HandshakeError::InvalidResponse)?;
            if frame
                != json!({"jsonrpc":"2.0","method":"process.exiting","params":{
                "protocol_version":"0.1.0","task_id":null,"exit_code":code}})
            {
                return Err(HandshakeError::InvalidResponse);
            }
        }
        Ok(())
    }
}

// Register both embedded resources so cross-schema references stay offline.
fn contract_validator(document: &str, definition: &str) -> jsonschema::Validator {
    let mut options = jsonschema::options();
    for source in [
        include_str!("../../../contracts/rpc.schema.json"),
        include_str!("../../../contracts/domain.schema.json"),
    ] {
        let schema: Value = serde_json::from_str(source).expect("embedded contract");
        let id = schema["$id"].as_str().expect("contract URI").to_owned();
        options = options.with_resource(id, jsonschema::Resource::from_contents(schema));
    }
    let reference = format!(
        "https://videosift.local/contracts/v0.1/{document}.schema.json#/$defs/{definition}"
    );
    options
        .build(&json!({"$ref":reference}))
        .expect("valid embedded definition")
}

static HANDSHAKE: LazyLock<jsonschema::Validator> =
    LazyLock::new(|| contract_validator("rpc", "HandshakeResult"));
static TRANSCRIPT: LazyLock<jsonschema::Validator> =
    LazyLock::new(|| contract_validator("domain", "Transcript"));

fn success_payload<'a>(frame: &'a Value, id: &str) -> Option<&'a Value> {
    if frame.as_object()?.len() != 3 || frame["jsonrpc"] != "2.0" || frame["id"] != id {
        return None;
    }
    frame.get("result")
}

async fn exchange(
    supervisor: &mut ProcessSupervisor,
    execution: Execution,
    frames: &mut Frames,
    mock: bool,
) -> Result<(), HandshakeError> {
    let mut request = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":"hello","method":"system.handshake",
        "params":{"protocol_version":"0.1.0","client_name":"core","task_id":null,"capabilities":[]}})).expect("static JSON");
    request.push(b'\n');
    supervisor.try_send_input(execution, &request)?;
    let frame = frames
        .next(supervisor, execution)
        .await?
        .ok_or(HandshakeError::UnexpectedExit)?;
    if let Some(result) = success_payload(&frame, "hello")
        && HANDSHAKE.is_valid(result)
        && result["server_name"] == "worker"
        && let Some(capabilities) = result["capabilities"].as_array()
        && (!mock || capabilities.iter().any(|cap| cap == "inference.transcribe"))
    {
        return Ok(());
    }
    if let Some(message) = frame.pointer("/error/message").and_then(Value::as_str)
        && !message.is_empty()
        && message.chars().count() <= 1024
        && frame
            == json!({"jsonrpc":"2.0","id":"hello","error":{"code":1009,"message":message,"data":{
                "code":"configuration","protocol_version":"0.1.0","message":message,"task_id":null,"recoverable":false,
                "details":{"reason":"protocol_version_mismatch"}}}})
    {
        return Err(HandshakeError::VersionMismatch);
    }
    Err(HandshakeError::InvalidResponse)
}

async fn transcribe(
    supervisor: &mut ProcessSupervisor,
    execution: Execution,
    frames: &mut Frames,
    task: &str,
    language: &str,
) -> Result<Value, HandshakeError> {
    let audio = json!({"artifact_path":"audio/mock.bin","size_bytes":3,"content_hash":blake3::hash(b"abc").to_hex().as_str()});
    let mut request = serde_json::to_vec(
        &json!({"jsonrpc":"2.0","method":"inference.transcribe","id":"transcribe","params":{
        "protocol_version":"0.1.0","task_id":task,"language":language,"audio":audio}}),
    )
    .expect("JSON values");
    request.push(b'\n');
    supervisor.try_send_input(execution, &request)?;
    let frame = frames
        .next(supervisor, execution)
        .await?
        .ok_or(HandshakeError::UnexpectedExit)?;
    let envelope = |payload: Value| json!({"jsonrpc":"2.0","id":"transcribe","error":payload});
    if let Some(message) = frame.pointer("/error/message").and_then(Value::as_str)
        && (1..=1024).contains(&message.chars().count())
    {
        if frame == envelope(json!({"code":-32602,"message":message})) {
            return Err(HandshakeError::InvalidParams);
        }
        if frame
            == envelope(
                json!({"code":1003,"message":message,"data":{"protocol_version":"0.1.0","task_id":task,"code":"source","message":message,"recoverable":false}}),
            )
        {
            return Err(HandshakeError::UnknownAudio);
        }
    }
    let result = success_payload(&frame, "transcribe").ok_or(HandshakeError::InvalidResponse)?;
    if !TRANSCRIPT.is_valid(result) || result["task_id"] != task || result["language"] != language {
        return Err(HandshakeError::InvalidResponse);
    }
    let mut evidence = HashSet::new();
    let mut previous = 0.0;
    for segment in result["segments"]
        .as_array()
        .ok_or(HandshakeError::InvalidResponse)?
    {
        // Schema bounds integer milliseconds to the exactly representable range of f64.
        let start = segment["start_ms"]
            .as_f64()
            .ok_or(HandshakeError::InvalidResponse)?;
        let end = segment["end_ms"]
            .as_f64()
            .ok_or(HandshakeError::InvalidResponse)?;
        if start < previous || end <= start || !evidence.insert(segment["evidence_id"].as_str()) {
            return Err(HandshakeError::InvalidResponse);
        }
        previous = start;
    }
    Ok(result.clone())
}
