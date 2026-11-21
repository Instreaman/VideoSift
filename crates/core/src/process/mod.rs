//! Bounded process execution layered over the accepted task scheduler.
//!
//! This is a byte transport and lifecycle API, not a Worker RPC implementation.
//! Programs/arguments must be trusted configuration. Outputs are untrusted bytes.

#[cfg(windows)]
mod job;
mod pipes;
mod runner;
mod tree;
#[cfg(unix)]
mod unix;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use crate::scheduler::{Execution, Outcome, Scheduler, SchedulerError, Snapshot, TaskHandle};
use crate::task::TaskState;

pub use pipes::{CHUNK_BYTES, OutputChunk, Stream};

pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub pipe_chunks: usize,
    pub run_timeout: Duration,
    pub cancel_grace: Duration,
    pub cleanup_timeout: Duration,
}

impl Limits {
    fn validate(self) -> Result<(), ProcessError> {
        let valid_duration =
            |duration: Duration| !duration.is_zero() && duration <= Duration::from_secs(86400);
        if !(1..=1024).contains(&self.pipe_chunks)
            || !valid_duration(self.run_timeout)
            || !valid_duration(self.cancel_grace)
            || !valid_duration(self.cleanup_timeout)
        {
            return Err(ProcessError::InvalidLimits);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    Exited { success: bool },
    Cancelled { grace_expired: bool },
    TimedOut,
    IoFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessReport {
    pub termination: Termination,
    /// A pipe task returned an error or panicked. Cleanup is tracked separately.
    pub pipe_failed: bool,
    pub cleanup_complete: bool,
}

#[derive(Debug)]
pub enum ProcessError {
    Scheduler(SchedulerError),
    InvalidLimits,
    InvalidProgram,
    NoRuntime,
    Spawn {
        execution: Execution,
        kind: std::io::ErrorKind,
    },
    NotManaged,
    InputFull,
    InputClosed,
    InputTooLarge,
    CleanupIncomplete,
    RunnerStopped,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Do not expose raw OS messages containing command arguments or paths.
        write!(formatter, "Process supervision error: {self:?}")
    }
}
impl std::error::Error for ProcessError {}
impl From<SchedulerError> for ProcessError {
    fn from(error: SchedulerError) -> Self {
        Self::Scheduler(error)
    }
}

struct Running {
    execution: Execution,
    input: Option<mpsc::Sender<Vec<u8>>>,
    output: Option<mpsc::Receiver<OutputChunk>>,
    cancel: watch::Sender<bool>,
    result: oneshot::Receiver<ProcessReport>,
    report: Option<ProcessReport>,
}

/// Single owner of scheduling and process settlement. Dropping it requests cancellation.
/// Keep the Tokio runtime alive until every process has been settled for confirmed cleanup.
pub struct ProcessSupervisor {
    scheduler: Scheduler,
    running: BTreeMap<TaskHandle, Running>,
}

impl ProcessSupervisor {
    pub fn new(capacity: usize, concurrency: usize) -> Result<Self, ProcessError> {
        Ok(Self {
            scheduler: Scheduler::new(capacity, concurrency)?,
            running: BTreeMap::new(),
        })
    }

    pub fn submit(&mut self) -> Result<TaskHandle, ProcessError> {
        Ok(self.scheduler.submit()?)
    }
    pub fn snapshot(&self, task: TaskHandle) -> Result<Snapshot, ProcessError> {
        Ok(self.scheduler.snapshot(task)?)
    }
    pub fn in_flight(&self) -> usize {
        self.scheduler.in_flight()
    }
    pub fn take_finished(&mut self, task: TaskHandle) -> Result<Snapshot, ProcessError> {
        Ok(self.scheduler.take_finished(task)?)
    }

    /// Dispatch the next step. A spawn failure marks that execution failed and frees its slot.
    /// Requires a Tokio runtime with I/O and time enabled. No shell is invoked.
    pub fn dispatch(
        &mut self,
        spec: ProcessSpec,
        limits: Limits,
    ) -> Result<Option<Execution>, ProcessError> {
        limits.validate()?;
        if !spec.program.is_absolute() {
            return Err(ProcessError::InvalidProgram);
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| ProcessError::NoRuntime)?;
        let Some(execution) = self.scheduler.dispatch()? else {
            return Ok(None);
        };
        let tree = match tree::ProcessTree::spawn(&spec) {
            Ok(tree) => tree,
            Err(error) => {
                self.scheduler.deliver(execution, Outcome::Failed)?;
                return Err(ProcessError::Spawn {
                    execution,
                    kind: error.kind(),
                });
            }
        };
        let (input_tx, input) = mpsc::channel(limits.pipe_chunks);
        let (output, output_rx) = mpsc::channel(limits.pipe_chunks);
        let (cancel_tx, cancel) = watch::channel(false);
        let (result, result_rx) = oneshot::channel();
        runtime.spawn(runner::run(
            tree,
            limits,
            runner::Channels {
                input,
                output,
                cancel,
                result,
            },
        ));
        self.running.insert(
            execution.task(),
            Running {
                execution,
                input: Some(input_tx),
                output: Some(output_rx),
                cancel: cancel_tx,
                result: result_rx,
                report: None,
            },
        );
        Ok(Some(execution))
    }

    fn check_execution(&self, execution: Execution) -> Result<(), ProcessError> {
        let matches = self
            .running
            .get(&execution.task())
            .is_some_and(|running| running.execution == execution);
        if !matches {
            return Err(ProcessError::Scheduler(SchedulerError::StaleExecution));
        }
        Ok(())
    }

    /// Nonblocking admission into the bounded stdin queue; retry InputFull later.
    pub fn try_send_input(&self, execution: Execution, bytes: &[u8]) -> Result<(), ProcessError> {
        let task = execution.task();
        self.check_execution(execution)?;
        if bytes.len() > CHUNK_BYTES {
            return Err(ProcessError::InputTooLarge);
        }
        let running = self.running.get(&task).ok_or(ProcessError::NotManaged)?;
        if self.scheduler.snapshot(task)?.state != TaskState::Running {
            return Err(ProcessError::InputClosed);
        }
        let input = running.input.as_ref().ok_or(ProcessError::InputClosed)?;
        input.try_send(bytes.to_vec()).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => ProcessError::InputFull,
            mpsc::error::TrySendError::Closed(_) => ProcessError::InputClosed,
        })
    }

    /// Deliver EOF after already queued input. Cancellation instead discards queued input.
    pub fn close_input(&mut self, execution: Execution) -> Result<(), ProcessError> {
        let task = execution.task();
        self.check_execution(execution)?;
        self.running
            .get_mut(&task)
            .ok_or(ProcessError::NotManaged)?
            .input
            .take();
        Ok(())
    }

    /// Consume while running. Accepted cancellation discards queued output and returns EOF.
    pub async fn recv_output(
        &mut self,
        execution: Execution,
    ) -> Result<Option<OutputChunk>, ProcessError> {
        let task = execution.task();
        self.check_execution(execution)?;
        let running = self
            .running
            .get_mut(&task)
            .ok_or(ProcessError::NotManaged)?;
        match running.output.as_mut() {
            Some(output) => Ok(output.recv().await),
            None => Ok(None),
        }
    }

    pub fn cancel(&mut self, task: TaskHandle) -> Result<TaskState, ProcessError> {
        let state = self.scheduler.cancel(task)?;
        if let Some(running) = self.running.get_mut(&task) {
            running.input.take();
            running.output.take();
            running.cancel.send_replace(true);
        }
        Ok(state)
    }

    /// Apply completion only after confirmed cleanup. Cancelling this future is safe:
    /// the runner keeps ownership and a later call can receive the same report.
    /// An incomplete cleanup quarantines the slot rather than claiming cancellation.
    pub async fn settle(&mut self, execution: Execution) -> Result<ProcessReport, ProcessError> {
        let task = execution.task();
        self.check_execution(execution)?;
        let running = self
            .running
            .get_mut(&task)
            .ok_or(ProcessError::NotManaged)?;
        let report = match running.report {
            Some(report) => report,
            None => {
                let report = (&mut running.result)
                    .await
                    .map_err(|_| ProcessError::RunnerStopped)?;
                running.report = Some(report);
                report
            }
        };
        if !report.cleanup_complete {
            return Err(ProcessError::CleanupIncomplete);
        }
        if self.scheduler.snapshot(task)?.state == TaskState::CancelRequested {
            self.scheduler.acknowledge_cancel(running.execution)?;
        } else {
            let outcome = if report.termination == (Termination::Exited { success: true }) {
                Outcome::Success
            } else {
                Outcome::Failed
            };
            self.scheduler.deliver(running.execution, outcome)?;
        }
        self.running.remove(&task);
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accepted_cancel_discards_queued_output_and_settles_even_after_pipe_error() {
        let mut supervisor = ProcessSupervisor::new(2, 1).unwrap();
        let task = supervisor.submit().unwrap();
        let next = supervisor.submit().unwrap();
        let execution = supervisor.scheduler.dispatch().unwrap().unwrap();
        let (result_tx, result) = oneshot::channel();
        let (cancel, _) = watch::channel(false);
        let (output_tx, output) = mpsc::channel(2);
        for bytes in [b"already read", b"still queued"] {
            output_tx
                .try_send(OutputChunk {
                    stream: Stream::Stdout,
                    bytes: bytes.to_vec(),
                })
                .unwrap();
        }
        supervisor.running.insert(
            task,
            Running {
                execution,
                input: None,
                output: Some(output),
                cancel,
                result,
                report: None,
            },
        );
        assert_eq!(
            supervisor
                .recv_output(execution)
                .await
                .unwrap()
                .unwrap()
                .bytes,
            b"already read"
        );
        assert_eq!(supervisor.cancel(task).unwrap(), TaskState::CancelRequested);
        assert!(output_tx.is_closed());
        assert!(supervisor.recv_output(execution).await.unwrap().is_none());
        assert_eq!(supervisor.in_flight(), 1);
        result_tx
            .send(ProcessReport {
                termination: Termination::Cancelled {
                    grace_expired: true,
                },
                pipe_failed: true,
                cleanup_complete: true,
            })
            .unwrap();
        assert!(supervisor.settle(execution).await.unwrap().pipe_failed);
        assert_eq!(
            supervisor.snapshot(task).unwrap().state,
            TaskState::Cancelled
        );
        assert_eq!(supervisor.in_flight(), 0);
        assert_eq!(
            supervisor.scheduler.dispatch().unwrap().unwrap().task(),
            next
        );
    }

    #[tokio::test]
    async fn incomplete_cleanup_quarantines_the_slot_even_after_cancel() {
        let mut supervisor = ProcessSupervisor::new(2, 1).unwrap();
        let task = supervisor.submit().unwrap();
        supervisor.submit().unwrap();
        let execution = supervisor.scheduler.dispatch().unwrap().unwrap();
        let (result_tx, result) = oneshot::channel();
        let (cancel, _) = watch::channel(false);
        let (_, output) = mpsc::channel(1);
        supervisor.running.insert(
            task,
            Running {
                execution,
                input: None,
                output: Some(output),
                cancel,
                result,
                report: None,
            },
        );
        // Inject the runner's cleanup-failure boundary, without leaking a real process.
        result_tx
            .send(ProcessReport {
                termination: Termination::TimedOut,
                pipe_failed: false,
                cleanup_complete: false,
            })
            .unwrap();
        supervisor.cancel(task).unwrap();
        for _ in 0..2 {
            assert!(matches!(
                supervisor.settle(execution).await,
                Err(ProcessError::CleanupIncomplete)
            ));
            assert_eq!(supervisor.in_flight(), 1);
            assert_eq!(
                supervisor.snapshot(task).unwrap().state,
                TaskState::CancelRequested
            );
            assert!(supervisor.scheduler.dispatch().unwrap().is_none());
        }
    }
}
