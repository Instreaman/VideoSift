//! Controlled in-process mock endpoints for the scheduler event loop.

use crate::scheduler::{Execution, Outcome, Scheduler, SchedulerError, Step};

pub struct MockWorker;
impl MockWorker {
    pub fn finish(
        scheduler: &mut Scheduler,
        execution: Execution,
        outcome: Outcome,
    ) -> Result<(), SchedulerError> {
        if execution.step() != Step::Worker {
            return Err(SchedulerError::WrongStep);
        }
        scheduler.deliver(execution, outcome)
    }
}
pub struct MockProvider;
impl MockProvider {
    pub fn finish(
        scheduler: &mut Scheduler,
        execution: Execution,
        outcome: Outcome,
    ) -> Result<(), SchedulerError> {
        if execution.step() != Step::Provider {
            return Err(SchedulerError::WrongStep);
        }
        scheduler.deliver(execution, outcome)
    }
}
