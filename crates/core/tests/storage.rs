use std::process::Command;
use std::sync::{Arc, Barrier};
use std::{env, fs, thread};

use rusqlite::Connection;
use tempfile::tempdir;
use videosift_core::storage::{StorageError, TaskStore};
use videosift_core::task::TaskState;

#[test]
fn cancellation_and_completion_follow_commit_order() {
    let dir = tempdir().unwrap();
    let mut first = TaskStore::open(dir.path()).unwrap();
    let mut second = first.writer().unwrap();

    let task = first.submit().unwrap();
    let ticket = first.dispatch(task).unwrap();
    first.save_checkpoint(&ticket, "worker-done").unwrap();
    assert_eq!(second.cancel(task).unwrap(), TaskState::CancelRequested);
    assert!(matches!(
        first.complete(&ticket, "final", "result"),
        Err(StorageError::Transition(_))
    ));
    let rejected = second.task(task).unwrap();
    assert_eq!(rejected.checkpoint.as_deref(), Some("worker-done"));
    assert_eq!(rejected.output_ref, None);
    first.acknowledge_cancel(&ticket).unwrap();
    assert_eq!(second.task(task).unwrap().state, TaskState::Cancelled);

    let task = first.submit().unwrap();
    let ticket = first.dispatch(task).unwrap();
    assert!(matches!(
        first.complete(&ticket, "final", " "),
        Err(StorageError::InvalidReference)
    ));
    assert_eq!(second.task(task).unwrap().checkpoint, None);
    first.complete(&ticket, "final", "result").unwrap();
    assert_eq!(second.cancel(task).unwrap(), TaskState::Succeeded);
    assert!(matches!(
        first.complete(&ticket, "other", "other"),
        Err(StorageError::StaleExecution)
    ));
    let committed = second.task(task).unwrap();
    assert_eq!(committed.output_ref.as_deref(), Some("result"));
    assert_eq!(committed.checkpoint.as_deref(), Some("final"));
}

#[test]
fn failed_commit_keeps_checkpoint_and_ticket_available_for_retry() {
    let dir = tempdir().unwrap();
    let mut writer = TaskStore::open(dir.path()).unwrap();
    let task = writer.submit().unwrap();
    let ticket = writer.dispatch(task).unwrap();
    writer.save_checkpoint(&ticket, "step-a").unwrap();
    // A live read transaction permits BEGIN IMMEDIATE but blocks COMMIT in rollback journal mode.
    let reader = Connection::open(dir.path().join("tasks.sqlite3")).unwrap();
    reader
        .execute_batch("BEGIN; SELECT state FROM processing_task;")
        .unwrap();
    let error = writer.complete(&ticket, "final", "result").unwrap_err();
    assert!(
        matches!(error, StorageError::Database(ref e) if e.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)),
        "{error}"
    );
    reader.execute_batch("COMMIT").unwrap();
    let previous = writer.task(task).unwrap();
    assert_eq!(previous.state, TaskState::Running);
    assert_eq!(previous.checkpoint.as_deref(), Some("step-a"));
    assert_eq!(previous.output_ref, None);
    writer.complete(&ticket, "final", "result").unwrap();
    assert_eq!(writer.task(task).unwrap().state, TaskState::Succeeded);
}

#[test]
fn simultaneous_writers_only_publish_the_winning_outcome() {
    let dir = tempdir().unwrap();
    let mut observer = TaskStore::open(dir.path()).unwrap();
    let task = observer.submit().unwrap();
    let ticket = observer.dispatch(task).unwrap();
    let mut canceller = observer.writer().unwrap();
    let mut completer = observer.writer().unwrap();
    let gate = Arc::new(Barrier::new(2));
    let start = gate.clone();
    let cancel = thread::spawn(move || {
        start.wait();
        canceller.cancel(task).unwrap()
    });
    gate.wait();
    let complete = completer.complete(&ticket, "final", "result");
    let state = cancel.join().unwrap();
    let record = observer.task(task).unwrap();
    assert_eq!(record.state, state);
    match state {
        TaskState::Succeeded => {
            complete.unwrap();
            assert_eq!(record.output_ref.as_deref(), Some("result"));
            assert_eq!(record.checkpoint.as_deref(), Some("final"));
        }
        TaskState::CancelRequested => {
            assert!(matches!(complete, Err(StorageError::Transition(_))));
            assert_eq!((record.output_ref, record.checkpoint), (None, None));
        }
        other => panic!("unexpected race outcome: {other:?}"),
    }
}

#[test]
fn restart_requires_explicit_resume_and_revokes_all_previous_tickets() {
    let dir = tempdir().unwrap();
    let mut store = TaskStore::open(dir.path()).unwrap();
    let mut active = Vec::new();
    for n in 0..3 {
        let id = store.submit().unwrap();
        let ticket = store.dispatch(id).unwrap();
        store.save_checkpoint(&ticket, "saved").unwrap();
        match n {
            1 => {
                store.pause(id).unwrap();
            }
            2 => {
                store.cancel(id).unwrap();
            }
            _ => {}
        }
        active.push((id, ticket));
    }
    let queued = store.submit().unwrap();
    let paused = store.submit().unwrap();
    store.pause(paused).unwrap();
    let finished = store.submit().unwrap();
    let ticket = store.dispatch(finished).unwrap();
    store.complete(&ticket, "final", "result").unwrap();
    let surviving_writer = store.writer().unwrap();
    drop(store);
    assert!(matches!(
        TaskStore::open(dir.path()),
        Err(StorageError::AlreadyOpen)
    ));
    assert_eq!(
        surviving_writer.task(active[0].0).unwrap().state,
        TaskState::Running
    );
    drop(surviving_writer);
    drop(TaskStore::open(dir.path()).unwrap()); // Recovery is idempotent across two restarts.
    let mut reopened = TaskStore::open(dir.path()).unwrap();
    assert_eq!(
        reopened.interrupted_after(0, 2).unwrap(),
        [active[0].0, active[1].0]
    );
    assert_eq!(
        reopened.interrupted_after(active[1].0, 2).unwrap(),
        [active[2].0]
    );
    assert_eq!(reopened.task(queued).unwrap().state, TaskState::Queued);
    assert_eq!(reopened.task(paused).unwrap().state, TaskState::Paused);
    assert_eq!(
        reopened.task(finished).unwrap().output_ref.as_deref(),
        Some("result")
    );
    for (id, old) in active {
        let recovered = reopened.task(id).unwrap();
        assert_eq!(recovered.state, TaskState::Interrupted);
        assert_eq!(recovered.checkpoint.as_deref(), Some("saved"));
        assert!(matches!(
            reopened.dispatch(id),
            Err(StorageError::Transition(_))
        ));
        assert!(matches!(
            reopened.complete(&old, "late", "old"),
            Err(StorageError::StaleExecution)
        ));
        assert_eq!(reopened.resume(id).unwrap(), TaskState::Queued);
        let current = reopened.dispatch(id).unwrap();
        assert!(matches!(
            reopened.acknowledge_cancel(&old),
            Err(StorageError::StaleExecution)
        ));
        assert!(matches!(
            reopened.save_checkpoint(&old, "late"),
            Err(StorageError::StaleExecution)
        ));
        assert!(matches!(
            reopened.complete(&old, "late", "old"),
            Err(StorageError::StaleExecution)
        ));
        assert_eq!(
            reopened.task(id).unwrap().checkpoint.as_deref(),
            Some("saved")
        );
        reopened.complete(&current, "fresh", "new").unwrap();
    }
}

#[test]
fn abrupt_process_exit_releases_ownership_and_recovers_committed_checkpoint() {
    const CHILD_DIR: &str = "VIDEOSIFT_STORAGE_TEST_CHILD_DIR";
    if let Some(path) = env::var_os(CHILD_DIR) {
        let mut store = TaskStore::open(path.as_ref()).unwrap();
        let task = store.submit().unwrap();
        let ticket = store.dispatch(task).unwrap();
        store.save_checkpoint(&ticket, "step-a").unwrap();
        std::process::exit(0); // Skip Rust destructors just as an abruptly stopped core would.
    }

    let dir = tempdir().unwrap();
    let result = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "abrupt_process_exit_releases_ownership_and_recovers_committed_checkpoint",
        ])
        .env(CHILD_DIR, dir.path())
        .status()
        .unwrap();
    assert!(result.success());
    let store = TaskStore::open(dir.path()).unwrap();
    let task = store.task(1).unwrap();
    assert_eq!(task.state, TaskState::Interrupted);
    assert_eq!(task.checkpoint.as_deref(), Some("step-a"));
    assert_eq!(task.output_ref, None);
}

#[test]
fn newer_version_committed_only_in_wal_survives_read_only_preflight() {
    const CHILD_DB: &str = "VIDEOSIFT_TEST_NEWER_WAL_DB";
    if let Some(path) = env::var_os(CHILD_DB) {
        let db = Connection::open(path).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            BEGIN; PRAGMA user_version=500; CREATE TABLE future_feature(value); COMMIT;",
        )
        .unwrap();
        std::process::exit(0);
    }
    let dir = tempdir().unwrap();
    drop(TaskStore::open(dir.path()).unwrap());
    let path = dir.path().join("tasks.sqlite3");
    let result = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "newer_version_committed_only_in_wal_survives_read_only_preflight",
        ])
        .env(CHILD_DB, &path)
        .status()
        .unwrap();
    assert!(result.success());
    let wal = dir.path().join("tasks.sqlite3-wal");
    let main_before = fs::read(&path).unwrap();
    assert_eq!(&main_before[60..64], &2_i32.to_be_bytes());
    let wal_before = fs::read(&wal).unwrap();
    assert!(wal_before.len() > 32);
    assert!(matches!(
        TaskStore::open(dir.path()),
        Err(StorageError::NewerVersion(500))
    ));
    assert!(
        fs::read(path).unwrap() == main_before,
        "main database changed"
    );
    assert!(fs::read(wal).unwrap() == wal_before, "WAL changed");
    assert!(!dir.path().join("backups").exists());
}

#[test]
fn hot_journal_is_checked_using_the_last_committed_version() {
    const CHILD_DB: &str = "VIDEOSIFT_TEST_HOT_JOURNAL_DB";
    if let Some(path) = env::var_os(CHILD_DB) {
        let db = Connection::open(path).unwrap();
        db.execute_batch(
            "PRAGMA cache_size=4; BEGIN IMMEDIATE; PRAGMA user_version=1;
            CREATE TABLE unfinished(data BLOB);
            WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<64)
            INSERT INTO unfinished SELECT zeroblob(8192) FROM n;",
        )
        .unwrap();
        std::process::exit(0);
    }
    for version in [2, 500] {
        let dir = tempdir().unwrap();
        let mut store = TaskStore::open(dir.path()).unwrap();
        let task = store.submit().unwrap();
        store.dispatch(task).unwrap();
        drop(store);
        let path = dir.path().join("tasks.sqlite3");
        let db = Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", version).unwrap();
        drop(db);
        let status = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "hot_journal_is_checked_using_the_last_committed_version",
            ])
            .env(CHILD_DB, &path)
            .status()
            .unwrap();
        assert!(status.success());
        let journal = dir.path().join("tasks.sqlite3-journal");
        let before = fs::read(&journal).unwrap();
        assert_ne!(&before[..8], &[0; 8]);
        let main_before = fs::read(&path).unwrap();
        let opened = TaskStore::open(dir.path());
        if version == 500 {
            assert!(matches!(opened, Err(StorageError::NewerVersion(500))));
            assert!(fs::read(path).unwrap() == main_before);
            assert!(fs::read(journal).unwrap() == before);
        } else {
            assert_eq!(
                opened.unwrap().task(task).unwrap().state,
                TaskState::Interrupted
            );
            assert!(!journal.exists());
        }
    }
}

#[test]
fn newer_or_unrecognized_database_is_left_intact() {
    for version in [500, 0, 1] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.sqlite3");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE sentinel(value); INSERT INTO sentinel VALUES ('keep');")
            .unwrap();
        db.pragma_update(None, "user_version", version).unwrap();
        drop(db);
        let before = fs::read(&path).unwrap();
        let error = TaskStore::open(dir.path()).err().unwrap();
        if version == 500 {
            assert!(matches!(error, StorageError::NewerVersion(500)));
        } else {
            assert!(matches!(error, StorageError::ForeignDatabase));
        }
        assert_eq!(fs::read(path).unwrap(), before);
        assert!(!dir.path().join("backups").exists());
    }
}
