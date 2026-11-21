//! A single foreground synthetic pipeline using the same scheduler, cache and task store.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::mock::{MockProvider, MockWorker};
use crate::scheduler::{Execution, Outcome, Scheduler, SchedulerError, Step, TaskHandle};
use crate::storage::cache::{ArtifactIdentity, CacheKey, CachedArtifact, Stage, StageIdentity};
use crate::storage::{ExecutionTicket, StorageError, TaskStore};

const FIXTURE: &str = "合成字幕：缓存与恢复。";

#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),
}

fn stage_key(stage: Stage, dependency: Option<ArtifactIdentity>) -> Result<CacheKey, StorageError> {
    let fingerprint = blake3::hash(FIXTURE.as_bytes()).to_hex();
    StageIdentity {
        source_id: "mock:phase-01",
        content_version: "1",
        content_fingerprint: fingerprint.as_str(),
        part_id: "1",
        stage,
        model: Some("mock"),
        model_version: Some("1"),
        algorithm_version: "1",
        prompt_version: None,
        schema_version: "1",
        dependencies: dependency
            .map(|id| BTreeMap::from([("subtitle", id)]))
            .unwrap_or_default(),
    }
    .key("{}")
}

fn dispatch(scheduler: &mut Scheduler, step: Step) -> Result<Execution, SchedulerError> {
    let execution = scheduler.dispatch()?.ok_or(SchedulerError::InvalidState)?;
    if execution.step() != step {
        return Err(SchedulerError::WrongStep);
    }
    Ok(execution)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageResult {
    CacheHit,
    Generated,
    CleanupPending,
}

/// Only invoke the Mock endpoint on a miss or a verified integrity failure.
fn stage_output(
    store: &mut TaskStore,
    key: &CacheKey,
    task: i64,
    generate: impl FnOnce() -> Result<Vec<u8>, SchedulerError>,
) -> Result<(CachedArtifact, StageResult), RunError> {
    match store.cache_get(key, Some(task)) {
        Ok(Some(hit)) => Ok((hit, StageResult::CacheHit)),
        Ok(None) => {
            let bytes = generate()?;
            let identity = store.cache_put(key, &bytes, Some(task))?;
            Ok((CachedArtifact { identity, bytes }, StageResult::Generated))
        }
        Err(StorageError::CacheIntegrity) => {
            let result = match store.repair_cache(key, &generate()?) {
                Ok(_) => StageResult::Generated,
                Err(StorageError::CacheCleanup(_)) => StageResult::CleanupPending,
                Err(err) => return Err(err.into()),
            };
            let repaired = store
                .cache_get(key, Some(task))?
                .ok_or(StorageError::CacheIntegrity)?;
            Ok((repaired, result))
        }
        Err(err) => Err(err.into()),
    }
}

/// Holds the foreground owner's ticket at the durable Worker/Provider boundary.
pub struct MockRun<'a> {
    store: &'a mut TaskStore,
    task: i64,
    ticket: ExecutionTicket,
    scheduler: Scheduler,
    handle: TaskHandle,
    worker_key: CacheKey,
    worker_identity: ArtifactIdentity,
    transcript: Vec<u8>,
    pub worker_result: StageResult,
}

impl<'a> MockRun<'a> {
    /// Dispatch a queued task. After an interruption the caller must explicitly resume it first.
    pub fn start(store: &'a mut TaskStore, task: i64) -> Result<Self, RunError> {
        let mut scheduler = Scheduler::new(1, 1)?;
        let handle = scheduler.submit()?;
        let execution = dispatch(&mut scheduler, Step::Worker)?;
        let ticket = store.dispatch(task)?;
        let worker_key = stage_key(Stage::Subtitle, None)?;
        let (artifact, worker_result) = stage_output(store, &worker_key, task, || {
            MockWorker::finish(&mut scheduler, execution, Outcome::Success)?;
            Ok(FIXTURE.as_bytes().to_vec())
        })?;
        if worker_result == StageResult::CacheHit {
            scheduler.deliver(execution, Outcome::Success)?;
        }
        store.save_checkpoint(&ticket, worker_key.as_str())?;
        Ok(Self {
            store,
            task,
            ticket,
            scheduler,
            handle,
            worker_key,
            worker_identity: artifact.identity,
            transcript: artifact.bytes,
            worker_result,
        })
    }

    /// Settle cancellation between steps, before a Provider can publish anything.
    pub fn cancel(mut self) -> Result<(), RunError> {
        self.store.cancel(self.task)?;
        self.scheduler.cancel(self.handle)?;
        self.store.acknowledge_cancel(&self.ticket)?;
        Ok(())
    }

    /// Publish the verified summary and report cache maintenance still pending.
    pub fn complete(mut self) -> Result<StageResult, RunError> {
        let execution = dispatch(&mut self.scheduler, Step::Provider)?;
        let key = stage_key(Stage::Summary, Some(self.worker_identity))?;
        let (_, result) = stage_output(self.store, &key, self.task, || {
            let mut summary = b"Mock summary: ".to_vec();
            summary.extend_from_slice(&self.transcript);
            MockProvider::finish(&mut self.scheduler, execution, Outcome::Success)?;
            Ok(summary)
        })?;
        if result == StageResult::CacheHit {
            self.scheduler.deliver(execution, Outcome::Success)?;
        }
        self.store
            .complete(&self.ticket, self.worker_key.as_str(), key.as_str())?;
        Ok(result)
    }
}
