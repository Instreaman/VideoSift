use std::io::{BufRead, Cursor};
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
    let mut frames = Frames::default();
    let mut result =
        match timeout_at(deadline, exchange(&mut supervisor, execution, &mut frames)).await {
            Ok(result) if Instant::now() < deadline => result,
            _ => Err(HandshakeError::Timeout),
        };
    if result.is_ok() {
        supervisor.close_input(execution)?;
        result = frames.drain(&mut supervisor, execution).await;
    }
    if result.is_err() {
        supervisor.cancel(task)?;
    }
    let report = supervisor.settle(execution).await?;
    result?;
    if report.pipe_failed || report.termination != (Termination::Exited { success: true }) {
        return Err(HandshakeError::UnexpectedExit);
    }
    Ok(())
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

async fn exchange(
    supervisor: &mut ProcessSupervisor,
    execution: Execution,
    frames: &mut Frames,
) -> Result<(), HandshakeError> {
    let mut request = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":"hello","method":"system.handshake",
        "params":{"protocol_version":"0.1.0","client_name":"core","task_id":null,"capabilities":[]}})).expect("static JSON");
    request.push(b'\n');
    supervisor.try_send_input(execution, &request)?;
    let frame = frames
        .next(supervisor, execution)
        .await?
        .ok_or(HandshakeError::UnexpectedExit)?;
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
    Err(HandshakeError::InvalidResponse)
}
