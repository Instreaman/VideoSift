//! SQLite ownership, schema upgrades and durable task transitions.

pub mod cache;
mod migrations;
mod publication;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OpenFlags, OptionalExtension, ToSql, TransactionBehavior, params};
use serde::Deserialize;
use serde::de::value::{Error as ValueError, StrDeserializer};
use thiserror::Error;

use crate::task::{TaskEvent, TaskState, TransitionError};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("SQLite task storage: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Accessing task storage files: {0}")]
    Io(#[from] std::io::Error),
    #[error("Another core owns this task database")]
    AlreadyOpen,
    #[error("Task database version {0} is newer than this core supports")]
    NewerVersion(i32),
    #[error("Unrecognized task database")]
    ForeignDatabase,
    #[error("Expected a task storage directory")]
    InvalidDirectory,
    #[error("Unknown task {0}")]
    UnknownTask(i64),
    #[error("Execution no longer owns this task")]
    StaleExecution,
    #[error("Expected a nonempty reference of at most 1024 bytes without control characters")]
    InvalidReference,
    #[error("Cache artifact is missing or failed integrity verification; regenerate the stage")]
    CacheIntegrity,
    #[error("An unfinished task still references different content for this cache key")]
    CacheInUse,
    #[error("Cache repair requires an indexed artifact that failed integrity checks")]
    CacheRepairNotNeeded,
    #[error("Settle live executions referencing the damaged cache artifact before repair")]
    CacheRepairBusy,
    #[error("Cache index committed; obsolete file cleanup needs retry: {0}")]
    CacheCleanup(#[source] std::io::Error),
    #[error("Invalid cache identity: {0}")]
    CacheIdentity(&'static str),
    #[error(transparent)]
    Transition(#[from] TransitionError),
}

/// SQLite TEXT uses the same state names as the public schema.
impl ToSql for TaskState {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        let name = match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::PauseRequested => "pause_requested",
            Self::Paused => "paused",
            Self::WaitingProvider => "waiting_provider",
            Self::CancelRequested => "cancel_requested",
            Self::Cancelled => "cancelled",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        };
        Ok(name.into())
    }
}

impl FromSql for TaskState {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        Self::deserialize(StrDeserializer::<ValueError>::new(value.as_str()?))
            .map_err(|error| FromSqlError::Other(Box::new(error)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTask {
    pub state: TaskState,
    pub checkpoint: Option<String>,
    pub output_ref: Option<String>,
}

/// Internal capability issued for one dispatch. Each retry receives a fresh nonce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTicket {
    task: i64,
    nonce: [u8; 16],
}

/// Synchronous core storage. A single engine owns restart recovery, even with multiple writers.
pub struct TaskStore {
    connection: Connection,
    path: PathBuf,
    owner: Arc<File>,
    startup_cache_cleanup_error: Option<StorageError>,
}

const MOCK_APPLICATION_ID: i32 = 0x56534D4B; // VSMK

impl TaskStore {
    /// Open an existing private application directory and commit task recovery.
    pub fn open(directory: &Path) -> Result<Self, StorageError> {
        Self::open_for(directory, migrations::APPLICATION_ID)
    }

    /// Only recover databases created for the synthetic CLI pipeline.
    pub fn open_mock(directory: &Path) -> Result<Self, StorageError> {
        Self::open_for(directory, MOCK_APPLICATION_ID)
    }

    fn open_for(directory: &Path, application_id: i32) -> Result<Self, StorageError> {
        let directory = directory.canonicalize()?;
        let owner = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("tasks.lock"))?;
        match owner.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(StorageError::AlreadyOpen),
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        let path = directory.join("tasks.sqlite3");
        if path.try_exists()? {
            let version = migrations::preflight(&path, application_id)?;
            if version == 0 && application_id == MOCK_APPLICATION_ID {
                return Err(StorageError::ForeignDatabase);
            }
        }
        let mut connection = Connection::open(&path)?;
        migrations::migrate(&mut connection, &path, application_id)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for state in [
            TaskState::Running,
            TaskState::PauseRequested,
            TaskState::CancelRequested,
        ] {
            let next = state.transition(TaskEvent::RecoverAfterRestart)?;
            tx.execute(
                "UPDATE processing_task SET state = ?1, execution = NULL WHERE state = ?2",
                params![next, state],
            )?;
        }
        tx.commit()?;
        let mut store = Self {
            connection,
            path,
            owner: Arc::new(owner),
            startup_cache_cleanup_error: None,
        };
        store.startup_cache_cleanup_error = store.cleanup_cache().err();
        Ok(store)
    }

    /// The cache cleanup error from opening this store, if cleanup still needs attention.
    pub fn startup_cache_cleanup_error(&self) -> Option<&StorageError> {
        self.startup_cache_cleanup_error.as_ref()
    }

    /// Independent transaction connection within the same core lifetime. Never runs recovery.
    pub fn writer(&self) -> Result<Self, StorageError> {
        Ok(Self {
            connection: Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE)?,
            path: self.path.clone(),
            owner: Arc::clone(&self.owner),
            startup_cache_cleanup_error: None,
        })
    }

    pub fn submit(&self) -> Result<i64, StorageError> {
        self.connection.execute(
            "INSERT INTO processing_task(state) VALUES (?1)",
            [TaskState::Queued],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn task(&self, task: i64) -> Result<StoredTask, StorageError> {
        self.connection
            .query_row(
                "SELECT state, checkpoint, output_ref FROM processing_task WHERE id = ?1",
                [task],
                |row| {
                    Ok(StoredTask {
                        state: row.get(0)?,
                        checkpoint: row.get(1)?,
                        output_ref: row.get(2)?,
                    })
                },
            )
            .optional()?
            .ok_or(StorageError::UnknownTask(task))
    }

    /// Discover interrupted tasks in stable pages. Pass zero for the first page.
    pub fn interrupted_after(&self, after: i64, limit: u16) -> Result<Vec<i64>, StorageError> {
        let mut query = self.connection.prepare(
            "SELECT id FROM processing_task WHERE state = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
        )?;
        Ok(query
            .query_map(params![TaskState::Interrupted, after, limit], |row| {
                row.get(0)
            })?
            .collect::<Result<_, _>>()?)
    }

    pub fn dispatch(&mut self, task: i64) -> Result<ExecutionTicket, StorageError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next = state(&tx, task)?.transition(TaskEvent::Dispatch)?;
        let nonce = tx.query_row(
            "UPDATE processing_task SET state = ?1, execution = randomblob(16)
            WHERE id = ?2 RETURNING execution",
            params![next, task],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(ExecutionTicket { task, nonce })
    }

    pub fn cancel(&mut self, task: i64) -> Result<TaskState, StorageError> {
        self.request(task, TaskEvent::RequestCancel)
    }

    pub fn pause(&mut self, task: i64) -> Result<TaskState, StorageError> {
        self.request(task, TaskEvent::RequestPause)
    }

    pub fn resume(&mut self, task: i64) -> Result<TaskState, StorageError> {
        self.request(task, TaskEvent::Resume)
    }

    fn request(&mut self, task: i64, event: TaskEvent) -> Result<TaskState, StorageError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next = state(&tx, task)?.transition(event)?;
        tx.execute(
            "UPDATE processing_task SET state = ?1 WHERE id = ?2",
            params![next, task],
        )?;
        tx.commit()?;
        Ok(next)
    }

    pub fn acknowledge_cancel(&mut self, ticket: &ExecutionTicket) -> Result<(), StorageError> {
        self.settle(ticket, TaskEvent::CancellationAcknowledged, None, None)
    }

    /// Record a validated step boundary without publishing a final result.
    pub fn save_checkpoint(
        &mut self,
        ticket: &ExecutionTicket,
        checkpoint: &str,
    ) -> Result<(), StorageError> {
        validate_reference(checkpoint)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // The states accepting a completion also accept a completed step.
        execution_state(&tx, ticket)?.transition(TaskEvent::Complete)?;
        tx.execute(
            "UPDATE processing_task SET checkpoint = ?1 WHERE id = ?2",
            params![checkpoint, ticket.task],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn acknowledge_pause(
        &mut self,
        ticket: &ExecutionTicket,
        checkpoint: &str,
    ) -> Result<(), StorageError> {
        validate_reference(checkpoint)?;
        self.settle(ticket, TaskEvent::PauseAcknowledged, Some(checkpoint), None)
    }

    /// The caller stages and validates the referenced artifacts before publication.
    pub fn complete(
        &mut self,
        ticket: &ExecutionTicket,
        checkpoint: &str,
        output_ref: &str,
    ) -> Result<(), StorageError> {
        validate_reference(checkpoint)?;
        validate_reference(output_ref)?;
        self.settle(
            ticket,
            TaskEvent::Complete,
            Some(checkpoint),
            Some(output_ref),
        )
    }

    fn settle(
        &mut self,
        ticket: &ExecutionTicket,
        event: TaskEvent,
        checkpoint: Option<&str>,
        output_ref: Option<&str>,
    ) -> Result<(), StorageError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next = execution_state(&tx, ticket)?.transition(event)?;
        tx.execute(
            "UPDATE processing_task SET state = ?1, execution = NULL,
            checkpoint = coalesce(?2, checkpoint), output_ref = ?3 WHERE id = ?4",
            params![next, checkpoint, output_ref, ticket.task],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn execution_state(
    connection: &Connection,
    ticket: &ExecutionTicket,
) -> Result<TaskState, StorageError> {
    connection
        .query_row(
            "SELECT state FROM processing_task WHERE id = ?1 AND execution = ?2",
            params![ticket.task, ticket.nonce],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StorageError::StaleExecution)
}

fn state(connection: &Connection, task: i64) -> Result<TaskState, StorageError> {
    connection
        .query_row(
            "SELECT state FROM processing_task WHERE id = ?1",
            [task],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StorageError::UnknownTask(task))
}

fn validate_reference(reference: &str) -> Result<(), StorageError> {
    if reference.trim().is_empty()
        || reference.len() > 1024
        || reference.chars().any(char::is_control)
    {
        return Err(StorageError::InvalidReference);
    }
    Ok(())
}
