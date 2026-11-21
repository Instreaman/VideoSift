use std::io;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until, timeout_at};

use super::pipes::{self, Control, OutputChunk, Stream};
use super::tree::ProcessTree;
use super::{Limits, ProcessReport, Termination};

pub(super) struct Channels {
    pub input: mpsc::Receiver<Vec<u8>>,
    pub output: mpsc::Sender<OutputChunk>,
    pub cancel: watch::Receiver<bool>,
    pub result: oneshot::Sender<ProcessReport>,
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow_and_update() {
            return;
        }
        // Dropping the owning supervisor also requests cleanup.
        if cancel.changed().await.is_err() {
            return;
        }
    }
}

async fn join_pumps(
    pumps: &mut JoinSet<io::Result<()>>,
    deadline: Instant,
    pipe_failed: &mut bool,
) -> bool {
    timeout_at(deadline, async {
        while let Some(joined) = pumps.join_next().await {
            *pipe_failed |= !matches!(joined, Ok(Ok(())));
        }
    })
    .await
    .is_ok()
}

pub(super) async fn run(mut tree: ProcessTree, limits: Limits, channels: Channels) {
    let Channels {
        input,
        output,
        mut cancel,
        result,
    } = channels;
    let (control, reader) = watch::channel(Control::default());
    let mut pumps = JoinSet::new();
    let mut pipe_failed = false;
    let setup = (|| -> io::Result<_> {
        let parent = tree.parent()?;
        Ok((
            parent
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("missing stdin"))?,
            parent
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("missing stdout"))?,
            parent
                .stderr
                .take()
                .ok_or_else(|| io::Error::other("missing stderr"))?,
        ))
    })();
    let (termination, kill_ok) = if let Ok((stdin, stdout, stderr)) = setup {
        pumps.spawn(pipes::write_input(stdin, input, reader.clone()));
        pumps.spawn(pipes::read_output(
            stdout,
            Stream::Stdout,
            output.clone(),
            reader.clone(),
        ));
        pumps.spawn(pipes::read_output(stderr, Stream::Stderr, output, reader));
        let deadline = Instant::now() + limits.run_timeout;
        let mut parent_status = None;
        let mut tree_killed = false;
        let termination = loop {
            if parent_status.is_some() && pumps.is_empty() {
                break Termination::Exited {
                    success: parent_status.unwrap_or(false),
                };
            }
            tokio::select! {
                biased;
                _ = cancelled(&mut cancel) => {
                    control.send_replace(Control { stop_input: true, discard_output: true });
                    let cooperative = if parent_status.is_some() {
                        true
                    } else {
                        matches!(timeout_at(Instant::now() + limits.cancel_grace, tree.wait_parent()).await, Ok(Ok(_)))
                    };
                    break Termination::Cancelled { grace_expired: !cooperative };
                },
                _ = sleep_until(deadline) => break Termination::TimedOut,
                status = tree.wait_parent(), if parent_status.is_none() => {
                    match status {
                        Ok(success) => {
                            parent_status = Some(success);
                            control.send_replace(Control { stop_input: true, discard_output: false });
                            // Even a successful parent must not leave descendants behind.
                            if tree.terminate().is_err() { break Termination::IoFailure; }
                            tree_killed = true;
                        },
                        Err(_) => break Termination::IoFailure,
                    }
                },
                joined = pumps.join_next(), if !pumps.is_empty() => {
                    if !matches!(joined, Some(Ok(Ok(())))) {
                        pipe_failed = true;
                        break Termination::IoFailure;
                    }
                },
            }
        };
        // A normal exit already drained outputs. Other exits deliberately discard them.
        control.send_replace(Control {
            stop_input: true,
            discard_output: true,
        });
        let kill_ok = tree_killed || tree.terminate().is_ok();
        (termination, kill_ok)
    } else {
        pipe_failed = true;
        (Termination::IoFailure, tree.terminate().is_ok())
    };

    let cleanup_deadline = Instant::now() + limits.cleanup_timeout;
    let tree_clean = kill_ok
        && matches!(
            timeout_at(cleanup_deadline, tree.wait_tree()).await,
            Ok(Ok(()))
        );
    let pipes_clean = join_pumps(&mut pumps, cleanup_deadline, &mut pipe_failed).await;
    // JoinSet aborts any remaining pipe tasks; no pipe buffer grows during cleanup.
    let _ = result.send(ProcessReport {
        termination,
        pipe_failed,
        cleanup_complete: tree_clean && pipes_clean,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cleanup_joins_remaining_pumps_after_a_broken_pipe() {
        let mut pumps = JoinSet::new();
        pumps.spawn(async { Err(io::ErrorKind::BrokenPipe.into()) });
        tokio::task::yield_now().await;
        let (completed, completion) = oneshot::channel();
        pumps.spawn(async {
            completed.send(()).unwrap();
            Ok(())
        });

        let mut pipe_failed = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        assert!(join_pumps(&mut pumps, deadline, &mut pipe_failed).await);
        assert!(pipe_failed);
        assert!(pumps.is_empty());
        completion.await.unwrap();
    }

    #[tokio::test]
    async fn deadline_still_reports_incomplete_cleanup_if_a_pump_has_not_ended() {
        let mut pumps = JoinSet::new();
        pumps.spawn(std::future::pending());
        let mut pipe_failed = false;
        assert!(!join_pumps(&mut pumps, Instant::now(), &mut pipe_failed).await);
        assert!(!pipe_failed);
        assert_eq!(pumps.len(), 1);
    }
}
