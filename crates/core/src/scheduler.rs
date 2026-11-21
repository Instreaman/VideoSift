//! Bounded in-memory scheduling with explicitly delivered execution results.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::task::{TaskEvent, TaskState};

static NEXT_SCHEDULER: AtomicU64 = AtomicU64::new(0);

/// Scheduler-local identity, distinct from public protocol task IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskHandle {
    scheduler: u64,
    id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Worker,
    Provider,
}

/// An opaque execution identity. Cloning permits testing duplicate/late delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Execution {
    task: TaskHandle,
    generation: u64,
    step: Step,
}
impl Execution {
    pub fn task(self) -> TaskHandle {
        self.task
    }
    pub fn step(self) -> Step {
        self.step
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failed,
    RateLimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub state: TaskState,
    pub step: Step,
    pub worker_completed: bool,
    pub result_published: bool,
}

struct Entry {
    snapshot: Snapshot,
    active: Option<Execution>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerError {
    InvalidCapacity,
    Full,
    UnknownTask,
    InvalidState,
    StaleExecution,
    WrongStep,
    IdExhausted,
}
impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Scheduler error: {self:?}")
    }
}
impl std::error::Error for SchedulerError {}

/// All mutations require exclusive access. Capacity includes retained terminal records.
/// `dispatch` and `deliver` form an explicit event loop with bounded in-flight steps.
/// Outputs are synthetic completion markers; persistence belongs to a later subtask.
pub struct Scheduler {
    scope: u64,
    capacity: usize,
    concurrency: usize,
    next_id: u64,
    entries: BTreeMap<TaskHandle, Entry>,
    ready: VecDeque<TaskHandle>,
    provider_paused: bool,
}
impl Scheduler {
    pub fn new(capacity: usize, concurrency: usize) -> Result<Self, SchedulerError> {
        if capacity == 0 || concurrency == 0 || concurrency > capacity {
            return Err(SchedulerError::InvalidCapacity);
        }
        let scope = NEXT_SCHEDULER
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| SchedulerError::IdExhausted)?;
        Ok(Self {
            scope,
            capacity,
            concurrency,
            next_id: 0,
            entries: BTreeMap::new(),
            ready: VecDeque::new(),
            provider_paused: false,
        })
    }
    fn allocate_id(&mut self) -> Result<u64, SchedulerError> {
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(SchedulerError::IdExhausted)?;
        Ok(self.next_id)
    }
    pub fn submit(&mut self) -> Result<TaskHandle, SchedulerError> {
        if self.entries.len() == self.capacity {
            return Err(SchedulerError::Full);
        }
        let task = TaskHandle {
            scheduler: self.scope,
            id: self.allocate_id()?,
        };
        self.entries.insert(
            task,
            Entry {
                snapshot: Snapshot {
                    state: TaskState::Queued,
                    step: Step::Worker,
                    worker_completed: false,
                    result_published: false,
                },
                active: None,
            },
        );
        self.ready.push_back(task);
        Ok(task)
    }
    pub fn snapshot(&self, task: TaskHandle) -> Result<Snapshot, SchedulerError> {
        Ok(self
            .entries
            .get(&task)
            .ok_or(SchedulerError::UnknownTask)?
            .snapshot)
    }
    pub fn in_flight(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.active.is_some())
            .count()
    }
    pub fn provider_paused(&self) -> bool {
        self.provider_paused
    }

    pub fn dispatch(&mut self) -> Result<Option<Execution>, SchedulerError> {
        if self.in_flight() >= self.concurrency {
            return Ok(None);
        }
        let position = self.ready.iter().position(|task| {
            self.entries
                .get(task)
                .is_some_and(|entry| !self.provider_paused || entry.snapshot.step == Step::Worker)
        });
        let Some(position) = position else {
            return Ok(None);
        };
        let generation = self.allocate_id()?;
        let task = self.ready[position];
        let entry = self
            .entries
            .get_mut(&task)
            .ok_or(SchedulerError::UnknownTask)?;
        if entry.snapshot.state == TaskState::Queued {
            entry.snapshot.state = entry
                .snapshot
                .state
                .transition(TaskEvent::Dispatch)
                .map_err(|_| SchedulerError::InvalidState)?;
        }
        let execution = Execution {
            task,
            generation,
            step: entry.snapshot.step,
        };
        entry.active = Some(execution);
        self.ready.remove(position);
        Ok(Some(execution))
    }

    /// Reject stale or cancelled outcomes before changing state or quota status.
    pub fn deliver(
        &mut self,
        execution: Execution,
        outcome: Outcome,
    ) -> Result<(), SchedulerError> {
        let entry = self
            .entries
            .get_mut(&execution.task)
            .ok_or(SchedulerError::StaleExecution)?;
        if entry.active != Some(execution) {
            return Err(SchedulerError::StaleExecution);
        }
        if entry.snapshot.state != TaskState::Running {
            return Err(SchedulerError::InvalidState);
        }
        if outcome == Outcome::RateLimited && execution.step != Step::Provider {
            return Err(SchedulerError::WrongStep);
        }
        let event = match (execution.step, outcome) {
            (Step::Worker, Outcome::Success) => None,
            (Step::Provider, Outcome::Success) => Some(TaskEvent::Complete),
            (_, Outcome::Failed) => Some(TaskEvent::Fail),
            (_, Outcome::RateLimited) => Some(TaskEvent::ProviderRateLimited),
        };
        if let Some(event) = event {
            entry.snapshot.state = entry
                .snapshot
                .state
                .transition(event)
                .map_err(|_| SchedulerError::InvalidState)?;
        }
        entry.active = None;
        match (execution.step, outcome) {
            (Step::Worker, Outcome::Success) => {
                entry.snapshot.worker_completed = true;
                entry.snapshot.step = Step::Provider;
                self.ready.push_back(execution.task);
            }
            (Step::Provider, Outcome::Success) => entry.snapshot.result_published = true,
            (_, Outcome::RateLimited) => self.provider_paused = true,
            (_, Outcome::Failed) => {}
        }
        Ok(())
    }

    /// Running work retains its slot until the matching cancellation acknowledgement.
    pub fn cancel(&mut self, task: TaskHandle) -> Result<TaskState, SchedulerError> {
        let entry = self
            .entries
            .get_mut(&task)
            .ok_or(SchedulerError::UnknownTask)?;
        let mut state = entry
            .snapshot
            .state
            .transition(TaskEvent::RequestCancel)
            .map_err(|_| SchedulerError::InvalidState)?;
        if state == TaskState::CancelRequested && entry.active.is_none() {
            state = state
                .transition(TaskEvent::CancellationAcknowledged)
                .map_err(|_| SchedulerError::InvalidState)?;
        }
        entry.snapshot.state = state;
        self.ready.retain(|queued| *queued != task);
        Ok(state)
    }
    pub fn acknowledge_cancel(&mut self, execution: Execution) -> Result<(), SchedulerError> {
        let entry = self
            .entries
            .get_mut(&execution.task)
            .ok_or(SchedulerError::StaleExecution)?;
        if entry.active != Some(execution) {
            return Err(SchedulerError::StaleExecution);
        }
        entry.snapshot.state = entry
            .snapshot
            .state
            .transition(TaskEvent::CancellationAcknowledged)
            .map_err(|_| SchedulerError::InvalidState)?;
        entry.active = None;
        Ok(())
    }
    /// Explicitly resume the provider gate and all tasks waiting on this provider.
    pub fn resume_provider(&mut self) -> Result<(), SchedulerError> {
        self.provider_paused = false;
        for (task, entry) in &mut self.entries {
            if entry.snapshot.state == TaskState::WaitingProvider {
                entry.snapshot.state = entry
                    .snapshot
                    .state
                    .transition(TaskEvent::Resume)
                    .map_err(|_| SchedulerError::InvalidState)?;
                self.ready.push_back(*task);
            }
        }
        Ok(())
    }
    /// Taking a terminal record releases admission capacity. Handles are never reused.
    pub fn take_finished(&mut self, task: TaskHandle) -> Result<Snapshot, SchedulerError> {
        let snapshot = self.snapshot(task)?;
        if !snapshot.state.is_terminal() {
            return Err(SchedulerError::InvalidState);
        }
        self.entries.remove(&task);
        Ok(snapshot)
    }
}
