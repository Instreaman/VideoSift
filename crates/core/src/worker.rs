use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};

use crate::process::{Limits, ProcessError, ProcessSpec, ProcessSupervisor, Stream, Termination};
use crate::scheduler::Execution;

#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error("worker handshake timed out")]
    Timeout,
    #[error("incompatible worker protocol")]
    VersionMismatch,
    #[error("invalid worker handshake response")]
    InvalidResponse,
    #[error("worker exited unexpectedly")]
    UnexpectedExit,
}

/// Probe a trusted isolated Python environment and Worker entry point, then reap the process.
pub async fn probe_worker(python: &Path, entry: &Path) -> Result<(), HandshakeError> {
    if !entry.is_absolute() {
        return Err(ProcessError::InvalidProgram.into());
    }
    let mut supervisor = ProcessSupervisor::new(1, 1)?;
    let task = supervisor.submit()?;
    let spec = ProcessSpec {
        program: python.into(),
        args: vec!["-I".into(), entry.into(), "--supervised".into()],
    };
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
    let result = match timeout_at(deadline, exchange(&mut supervisor, execution)).await {
        Ok(result) if Instant::now() < deadline => result,
        _ => Err(HandshakeError::Timeout),
    };
    if result.is_ok() {
        supervisor.close_input(execution)?;
    } else {
        supervisor.cancel(task)?;
    }
    let report = supervisor.settle(execution).await?;
    result?;
    if report.pipe_failed || report.termination != (Termination::Exited { success: true }) {
        return Err(HandshakeError::UnexpectedExit);
    }
    Ok(())
}

async fn exchange(
    supervisor: &mut ProcessSupervisor,
    execution: Execution,
) -> Result<(), HandshakeError> {
    let mut request = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":"hello","method":"system.handshake",
        "params":{"protocol_version":"0.1.0","client_name":"core","task_id":null,"capabilities":[]}})).expect("static JSON");
    request.push(b'\n');
    supervisor.try_send_input(execution, &request)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = supervisor.recv_output(execution).await? {
        if chunk.stream == Stream::Stderr {
            continue;
        }
        if bytes.len() + chunk.bytes.len() > 1_048_576 {
            return Err(HandshakeError::InvalidResponse);
        }
        let complete = chunk.bytes.contains(&b'\n');
        bytes.extend(chunk.bytes);
        if !complete {
            continue;
        }
        if bytes.iter().position(|b| *b == b'\n') != Some(bytes.len() - 1) {
            return Err(HandshakeError::InvalidResponse);
        }
        let frame: Value =
            serde_json::from_slice(&bytes).map_err(|_| HandshakeError::InvalidResponse)?;
        if frame
            == json!({"jsonrpc":"2.0","id":"hello","result":{"server_name":"worker","protocol_version":"0.1.0","task_id":null,"capabilities":[]}})
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
        return Err(HandshakeError::InvalidResponse);
    }
    Err(HandshakeError::UnexpectedExit)
}
