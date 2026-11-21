//! Fixed-size chunks and bounded channels propagate pressure back to OS pipes.

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

pub const CHUNK_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Debug, PartialEq, Eq)]
pub struct OutputChunk {
    pub stream: Stream,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Control {
    pub stop_input: bool,
    pub discard_output: bool,
}

pub(super) async fn stopping(control: &mut watch::Receiver<Control>, input: bool) {
    loop {
        let value = *control.borrow_and_update();
        if (input && value.stop_input) || (!input && value.discard_output) {
            return;
        }
        if control.changed().await.is_err() {
            return;
        }
    }
}

pub(super) async fn read_output(
    mut pipe: impl AsyncRead + Unpin,
    stream: Stream,
    output: mpsc::Sender<OutputChunk>,
    mut control: watch::Receiver<Control>,
) -> io::Result<()> {
    let mut buffer = [0; CHUNK_BYTES];
    loop {
        let count = pipe.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        if control.borrow().discard_output || output.is_closed() {
            continue;
        }
        let chunk = OutputChunk {
            stream,
            bytes: buffer[..count].to_vec(),
        };
        tokio::select! {
            biased;
            _ = stopping(&mut control, false) => {},
            // The owner may close the receiver during cancellation. Continue draining
            // the OS pipe so the child can finish cooperating without blocking.
            _ = output.send(chunk) => {},
        }
    }
}

pub(super) async fn write_input(
    mut pipe: impl AsyncWrite + Unpin,
    mut input: mpsc::Receiver<Vec<u8>>,
    mut control: watch::Receiver<Control>,
) -> io::Result<()> {
    let write = async {
        while let Some(chunk) = input.recv().await {
            pipe.write_all(&chunk).await?;
        }
        pipe.shutdown().await
    };
    tokio::select! {
        biased;
        _ = stopping(&mut control, true) => Ok(()),
        result = write => result,
    }
    // Dropping ChildStdin closes it even if a write was blocked.
}
