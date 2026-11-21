//! Pure state transitions. The caller owns task data and atomic persistence.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Wire values match ProcessingTask.state in the shared domain schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    PauseRequested,
    Paused,
    WaitingProvider,
    CancelRequested,
    Cancelled,
    Succeeded,
    Failed,
    Interrupted,
}

/// Internal lifecycle events, not additional JSON-RPC methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskEvent {
    Dispatch,
    RequestPause,
    PauseAcknowledged,
    Resume,
    RequestCancel,
    CancellationAcknowledged,
    Complete,
    Fail,
    ProviderRateLimited,
    RecoverAfterRestart,
}

/// An event is invalid for the current state. No state has been changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionError {
    pub state: TaskState,
    pub event: TaskEvent,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Cannot apply {:?} to {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for TransitionError {}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Cancelled | Self::Succeeded | Self::Failed)
    }

    /// Compute the next state without mutating the task.
    ///
    /// The scheduler must serialize events and commit the returned state together
    /// with the associated errors, checkpoints and output references. For `Complete`,
    /// stage and validate outputs before starting the transaction. Within the
    /// transaction, apply the event to the current state and, if accepted, publish
    /// output references and checkpoints together with `Succeeded`. If cancellation
    /// was already accepted, reject the completion and leave the results unpublished.
    /// Recovery must run before accepting work after a restart.
    pub fn transition(self, event: TaskEvent) -> Result<Self, TransitionError> {
        use TaskEvent as Event;

        let next = match (self, event) {
            (Self::Queued, Event::Dispatch) => Self::Running,
            (Self::Queued, Event::RequestPause) => Self::Paused,
            (Self::Running, Event::RequestPause) => Self::PauseRequested,
            (Self::PauseRequested | Self::Paused, Event::RequestPause) => self,
            (Self::PauseRequested, Event::PauseAcknowledged) => Self::Paused,
            (Self::Paused | Self::WaitingProvider | Self::Interrupted, Event::Resume) => {
                Self::Queued
            }
            (Self::Queued | Self::Running, Event::Resume) => self,
            (Self::Running | Self::PauseRequested, Event::RequestCancel) => Self::CancelRequested,
            (
                Self::Queued | Self::Paused | Self::WaitingProvider | Self::Interrupted,
                Event::RequestCancel,
            ) => Self::Cancelled,
            (
                Self::CancelRequested | Self::Cancelled | Self::Succeeded | Self::Failed,
                Event::RequestCancel,
            ) => self,
            (Self::CancelRequested, Event::CancellationAcknowledged) => Self::Cancelled,
            (Self::Running | Self::PauseRequested, Event::Complete) => Self::Succeeded,
            (Self::Running | Self::PauseRequested, Event::Fail) => Self::Failed,
            (Self::Running | Self::PauseRequested, Event::ProviderRateLimited) => {
                Self::WaitingProvider
            }
            (
                Self::Running | Self::PauseRequested | Self::CancelRequested,
                Event::RecoverAfterRestart,
            ) => Self::Interrupted,
            (_, Event::RecoverAfterRestart) => self,
            _ => return Err(TransitionError { state: self, event }),
        };
        Ok(next)
    }
}
