use videosift_core::mock::{MockProvider, MockWorker};
use videosift_core::scheduler::{Outcome, Scheduler, SchedulerError, Step};
use videosift_core::task::TaskState;

#[test]
fn bounded_admission_fifo_and_running_slots() {
    assert!(Scheduler::new(0, 1).is_err());
    assert!(Scheduler::new(1, 0).is_err());
    assert!(Scheduler::new(1, 2).is_err());
    let mut scheduler = Scheduler::new(2, 1).unwrap();
    let first = scheduler.submit().unwrap();
    let second = scheduler.submit().unwrap();
    assert_eq!(scheduler.submit(), Err(SchedulerError::Full));
    let execution = scheduler.dispatch().unwrap().unwrap();
    assert_eq!(execution.task(), first);
    assert_eq!(scheduler.in_flight(), 1);
    assert!(scheduler.dispatch().unwrap().is_none());
    MockWorker::finish(&mut scheduler, execution, Outcome::Success).unwrap();
    let next = scheduler.dispatch().unwrap().unwrap();
    assert_eq!(next.task(), second);
    MockWorker::finish(&mut scheduler, next, Outcome::Failed).unwrap();
    assert_eq!(scheduler.snapshot(second).unwrap().state, TaskState::Failed);
    assert_eq!(scheduler.submit(), Err(SchedulerError::Full));
    scheduler.take_finished(second).unwrap();
    let third = scheduler.submit().unwrap();
    assert_ne!(third, second);
    let provider = scheduler.dispatch().unwrap().unwrap();
    assert_eq!(provider.task(), first);
    assert_eq!(provider.step(), Step::Provider);
    MockProvider::finish(&mut scheduler, provider, Outcome::Success).unwrap();
    let snapshot = scheduler.take_finished(first).unwrap();
    assert!(snapshot.worker_completed && snapshot.result_published);
    assert_eq!(snapshot.state, TaskState::Succeeded);
    assert_eq!(scheduler.dispatch().unwrap().unwrap().task(), third);
}

#[test]
fn cancel_first_rejects_completion_and_holds_slot_until_ack() {
    let mut scheduler = Scheduler::new(2, 1).unwrap();
    let task = scheduler.submit().unwrap();
    let other = scheduler.submit().unwrap();
    let worker = scheduler.dispatch().unwrap().unwrap();
    MockWorker::finish(&mut scheduler, worker, Outcome::Success).unwrap();
    let other_worker = scheduler.dispatch().unwrap().unwrap();
    MockWorker::finish(&mut scheduler, other_worker, Outcome::Failed).unwrap();
    let provider = scheduler.dispatch().unwrap().unwrap();
    assert_eq!(scheduler.cancel(task).unwrap(), TaskState::CancelRequested);
    assert_eq!(scheduler.cancel(task).unwrap(), TaskState::CancelRequested);
    for outcome in [Outcome::Success, Outcome::Failed, Outcome::RateLimited] {
        assert_eq!(
            MockProvider::finish(&mut scheduler, provider, outcome),
            Err(SchedulerError::InvalidState)
        );
    }
    assert!(!scheduler.provider_paused());
    assert!(!scheduler.snapshot(task).unwrap().result_published);
    assert_eq!(scheduler.in_flight(), 1);
    assert!(scheduler.dispatch().unwrap().is_none());
    scheduler.acknowledge_cancel(provider).unwrap();
    assert_eq!(scheduler.in_flight(), 0);
    assert_eq!(
        scheduler.snapshot(task).unwrap().state,
        TaskState::Cancelled
    );
    assert_eq!(scheduler.cancel(task).unwrap(), TaskState::Cancelled);
    assert_eq!(
        scheduler.acknowledge_cancel(provider),
        Err(SchedulerError::StaleExecution)
    );
    scheduler.take_finished(other).unwrap();
    scheduler.submit().unwrap();
    assert!(scheduler.dispatch().unwrap().is_some());
}

#[test]
fn completion_first_preserves_published_result_and_rejects_duplicates() {
    let mut scheduler = Scheduler::new(1, 1).unwrap();
    let task = scheduler.submit().unwrap();
    let worker = scheduler.dispatch().unwrap().unwrap();
    MockWorker::finish(&mut scheduler, worker, Outcome::Success).unwrap();
    assert_eq!(
        MockWorker::finish(&mut scheduler, worker, Outcome::Success),
        Err(SchedulerError::StaleExecution)
    );
    let provider = scheduler.dispatch().unwrap().unwrap();
    MockProvider::finish(&mut scheduler, provider, Outcome::Success).unwrap();
    assert_eq!(scheduler.cancel(task).unwrap(), TaskState::Succeeded);
    assert!(scheduler.snapshot(task).unwrap().result_published);
    assert_eq!(
        MockProvider::finish(&mut scheduler, provider, Outcome::RateLimited),
        Err(SchedulerError::StaleExecution)
    );
    assert!(!scheduler.provider_paused());
    scheduler.take_finished(task).unwrap();
    assert_eq!(
        scheduler.deliver(provider, Outcome::Success),
        Err(SchedulerError::StaleExecution)
    );
}

#[test]
fn provider_limit_blocks_new_calls_but_allows_worker_and_explicit_resume() {
    let mut scheduler = Scheduler::new(3, 1).unwrap();
    let first = scheduler.submit().unwrap();
    let worker = scheduler.dispatch().unwrap().unwrap();
    MockWorker::finish(&mut scheduler, worker, Outcome::Success).unwrap();
    let old = scheduler.dispatch().unwrap().unwrap();
    MockProvider::finish(&mut scheduler, old, Outcome::RateLimited).unwrap();
    assert!(scheduler.provider_paused());
    assert_eq!(
        scheduler.snapshot(first).unwrap().state,
        TaskState::WaitingProvider
    );
    assert!(scheduler.snapshot(first).unwrap().worker_completed);
    let second = scheduler.submit().unwrap();
    let worker = scheduler.dispatch().unwrap().unwrap();
    assert_eq!(worker.task(), second);
    MockWorker::finish(&mut scheduler, worker, Outcome::Success).unwrap();
    assert!(scheduler.dispatch().unwrap().is_none());
    scheduler.resume_provider().unwrap();
    scheduler.resume_provider().unwrap();
    let second_provider = scheduler.dispatch().unwrap().unwrap();
    MockProvider::finish(&mut scheduler, second_provider, Outcome::Success).unwrap();
    let retry = scheduler.dispatch().unwrap().unwrap();
    assert_eq!(retry.task(), first);
    assert_ne!(retry, old);
    assert_eq!(
        scheduler.deliver(old, Outcome::Success),
        Err(SchedulerError::StaleExecution)
    );
    assert_eq!(
        scheduler.deliver(old, Outcome::RateLimited),
        Err(SchedulerError::StaleExecution)
    );
    assert!(!scheduler.provider_paused());
    MockProvider::finish(&mut scheduler, retry, Outcome::Success).unwrap();
    assert!(scheduler.dispatch().unwrap().is_none());
    assert!(scheduler.snapshot(first).unwrap().result_published);
}

#[test]
fn cancel_queued_between_steps_and_waiting_provider() {
    let mut scheduler = Scheduler::new(1, 1).unwrap();
    let queued = scheduler.submit().unwrap();
    scheduler.cancel(queued).unwrap();
    assert!(scheduler.dispatch().unwrap().is_none());
    scheduler.take_finished(queued).unwrap();
    let between = scheduler.submit().unwrap();
    let worker = scheduler.dispatch().unwrap().unwrap();
    MockWorker::finish(&mut scheduler, worker, Outcome::Success).unwrap();
    assert_eq!(scheduler.cancel(between).unwrap(), TaskState::Cancelled);
    assert!(scheduler.dispatch().unwrap().is_none());
    scheduler.take_finished(between).unwrap();
    let waiting = scheduler.submit().unwrap();
    let worker = scheduler.dispatch().unwrap().unwrap();
    MockWorker::finish(&mut scheduler, worker, Outcome::Success).unwrap();
    let provider = scheduler.dispatch().unwrap().unwrap();
    MockProvider::finish(&mut scheduler, provider, Outcome::RateLimited).unwrap();
    assert_eq!(scheduler.cancel(waiting).unwrap(), TaskState::Cancelled);
    scheduler.resume_provider().unwrap();
    assert!(scheduler.dispatch().unwrap().is_none());
}

#[test]
fn concurrency_bound_and_wrong_endpoint_do_not_mutate_state() {
    let mut scheduler = Scheduler::new(3, 2).unwrap();
    let first = scheduler.submit().unwrap();
    scheduler.submit().unwrap();
    scheduler.submit().unwrap();
    let worker = scheduler.dispatch().unwrap().unwrap();
    let second = scheduler.dispatch().unwrap().unwrap();
    assert_eq!(scheduler.in_flight(), 2);
    assert!(scheduler.dispatch().unwrap().is_none());
    assert_eq!(
        scheduler.take_finished(first),
        Err(SchedulerError::InvalidState)
    );
    assert_eq!(
        MockProvider::finish(&mut scheduler, worker, Outcome::Success),
        Err(SchedulerError::WrongStep)
    );
    assert_eq!(
        MockWorker::finish(&mut scheduler, worker, Outcome::RateLimited),
        Err(SchedulerError::WrongStep)
    );
    assert_eq!(scheduler.in_flight(), 2);
    MockWorker::finish(&mut scheduler, second, Outcome::Failed).unwrap();
    assert!(scheduler.dispatch().unwrap().is_some());
}

#[test]
fn foreign_scheduler_handles_and_executions_are_rejected() {
    let mut first = Scheduler::new(1, 1).unwrap();
    let mut second = Scheduler::new(1, 1).unwrap();
    let foreign = first.submit().unwrap();
    second.submit().unwrap();
    let execution = first.dispatch().unwrap().unwrap();
    second.dispatch().unwrap();
    assert_eq!(second.cancel(foreign), Err(SchedulerError::UnknownTask));
    assert_eq!(
        second.deliver(execution, Outcome::Success),
        Err(SchedulerError::StaleExecution)
    );
    assert_eq!(second.in_flight(), 1);
}

#[test]
fn in_flight_provider_can_finish_during_quota_pause() {
    let mut scheduler = Scheduler::new(2, 2).unwrap();
    let first = scheduler.submit().unwrap();
    let second = scheduler.submit().unwrap();
    let a = scheduler.dispatch().unwrap().unwrap();
    let b = scheduler.dispatch().unwrap().unwrap();
    MockWorker::finish(&mut scheduler, a, Outcome::Success).unwrap();
    MockWorker::finish(&mut scheduler, b, Outcome::Success).unwrap();
    let a = scheduler.dispatch().unwrap().unwrap();
    let b = scheduler.dispatch().unwrap().unwrap();
    MockProvider::finish(&mut scheduler, a, Outcome::RateLimited).unwrap();
    MockProvider::finish(&mut scheduler, b, Outcome::Success).unwrap();
    assert!(scheduler.provider_paused());
    assert_eq!(
        scheduler.snapshot(first).unwrap().state,
        TaskState::WaitingProvider
    );
    assert_eq!(
        scheduler.snapshot(second).unwrap().state,
        TaskState::Succeeded
    );
    assert!(scheduler.dispatch().unwrap().is_none());
    scheduler.resume_provider().unwrap();
    let retry = scheduler.dispatch().unwrap().unwrap();
    MockProvider::finish(&mut scheduler, retry, Outcome::Failed).unwrap();
    assert_eq!(scheduler.snapshot(first).unwrap().state, TaskState::Failed);
    assert!(!scheduler.snapshot(first).unwrap().result_published);
}

#[test]
fn cancelling_worker_never_enqueues_provider() {
    let mut scheduler = Scheduler::new(1, 1).unwrap();
    let task = scheduler.submit().unwrap();
    let worker = scheduler.dispatch().unwrap().unwrap();
    assert_eq!(
        scheduler.acknowledge_cancel(worker),
        Err(SchedulerError::InvalidState)
    );
    scheduler.cancel(task).unwrap();
    assert_eq!(
        MockWorker::finish(&mut scheduler, worker, Outcome::Success),
        Err(SchedulerError::InvalidState)
    );
    scheduler.acknowledge_cancel(worker).unwrap();
    assert!(scheduler.dispatch().unwrap().is_none());
    let snapshot = scheduler.take_finished(task).unwrap();
    assert!(!snapshot.worker_completed && !snapshot.result_published);
}
