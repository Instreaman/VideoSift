use std::{fs, path::PathBuf};

use rusqlite::{Connection, ErrorCode};
use tempfile::{TempDir, tempdir};

use super::{APPLICATION_ID, MIGRATIONS, migrate, migrate_with_publisher, preflight, publication};
use crate::storage::{StorageError, TaskStore};
use crate::task::TaskState;

fn version_one() -> (TempDir, PathBuf, Connection) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("tasks.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(MIGRATIONS[0]).unwrap();
    db.pragma_update(None, "application_id", APPLICATION_ID)
        .unwrap();
    db.pragma_update(None, "user_version", 1).unwrap();
    (dir, path, db)
}

fn assert_version_one(db: &Connection) {
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        1
    );
    let columns: i32 = db
        .query_row(
            "SELECT count(*) FROM pragma_table_info('processing_task')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(columns, 2);
    let state: TaskState = db
        .query_row("SELECT state FROM processing_task WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(state, TaskState::Running);
}

#[test]
fn version_two_upgrade_preserves_task_outputs_and_creates_empty_cache() {
    let (dir, path, db) = version_one();
    db.execute_batch(MIGRATIONS[1]).unwrap();
    db.pragma_update(None, "user_version", 2).unwrap();
    db.execute_batch(
        "INSERT INTO processing_task(state, checkpoint, output_ref)
        VALUES ('succeeded', 'last-step', 'result')",
    )
    .unwrap();
    drop(db);
    let store = TaskStore::open(dir.path()).unwrap();
    let task = store.task(1).unwrap();
    assert_eq!(task.state, TaskState::Succeeded);
    assert_eq!(task.checkpoint.as_deref(), Some("last-step"));
    assert_eq!(task.output_ref.as_deref(), Some("result"));
    let backups = backups(&dir);
    assert_eq!(backups.len(), 1);
    let backup = Connection::open(&backups[0]).unwrap();
    assert_eq!(
        backup
            .pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        2
    );
    let db = Connection::open(path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM cache_entry", [], |r| r
            .get::<_, i32>(0))
            .unwrap(),
        0
    );
}

fn backups(dir: &TempDir) -> Vec<PathBuf> {
    fs::read_dir(dir.path().join("backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect()
}

#[test]
fn migration_backs_up_committed_wal_rows_before_schema_change_and_recovery() {
    let (dir, _path, db) = version_one();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    let states = [
        TaskState::Running,
        TaskState::Paused,
        TaskState::Queued,
        TaskState::Failed,
        TaskState::WaitingProvider,
        TaskState::Cancelled,
        TaskState::Succeeded,
        TaskState::Interrupted,
        TaskState::PauseRequested,
        TaskState::CancelRequested,
    ];
    for state in states {
        db.execute("INSERT INTO processing_task(state) VALUES (?1)", [state])
            .unwrap();
    }
    let store = TaskStore::open(dir.path()).unwrap();
    for (index, state) in states.into_iter().enumerate() {
        assert_eq!(
            store.task(index as i64 + 1).unwrap().state,
            state
                .transition(crate::task::TaskEvent::RecoverAfterRestart)
                .unwrap()
        );
    }
    let files = backups(&dir);
    assert_eq!(files.len(), 1);
    assert_version_one(&Connection::open(&files[0]).unwrap());
    drop(store);
    drop(TaskStore::open(dir.path()).unwrap());
    assert_eq!(backups(&dir), files);
}

#[test]
fn sqlite_full_rolls_back_ddl_and_preserves_pre_upgrade_snapshot_on_retry() {
    let (dir, path, mut db) = version_one();
    db.execute("INSERT INTO processing_task(state) VALUES ('running')", [])
        .unwrap();
    let pages: i64 = db
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    db.pragma_update(None, "max_page_count", pages).unwrap();
    let error = migrate(&mut db, &path, APPLICATION_ID).unwrap_err();
    assert!(
        matches!(error, StorageError::Database(ref e) if e.sqlite_error_code() == Some(ErrorCode::DiskFull)),
        "{error}"
    );
    assert_version_one(&db);
    let files = backups(&dir);
    assert_eq!(files.len(), 1);
    let before = fs::read(&files[0]).unwrap();
    assert_version_one(&Connection::open(&files[0]).unwrap());
    drop(db);
    let store = TaskStore::open(dir.path()).unwrap();
    assert_eq!(store.task(1).unwrap().state, TaskState::Interrupted);
    assert_eq!(backups(&dir).len(), 2);
    assert_eq!(fs::read(&files[0]).unwrap(), before);
}

#[test]
fn backup_destination_failure_prevents_any_schema_change() {
    let (dir, path, mut db) = version_one();
    db.execute("INSERT INTO processing_task(state) VALUES ('running')", [])
        .unwrap();
    fs::write(dir.path().join("backups"), b"occupied").unwrap();
    assert!(matches!(
        migrate(&mut db, &path, APPLICATION_ID),
        Err(StorageError::Io(_))
    ));
    assert_version_one(&db);
}

#[test]
fn publication_collision_preserves_both_old_database_and_existing_backup() {
    let (dir, path, mut db) = version_one();
    db.execute("INSERT INTO processing_task(state) VALUES ('running')", [])
        .unwrap();
    let error = migrate_with_publisher(&mut db, &path, APPLICATION_ID, |temp, destination| {
        fs::write(destination, b"reserved filename")?;
        publication::publish(temp, destination)
    })
    .unwrap_err();
    assert!(matches!(error, StorageError::Io(_)), "{error}");
    assert_version_one(&db);
    let files = backups(&dir);
    assert_eq!(files.len(), 1);
    assert_eq!(fs::read(&files[0]).unwrap(), b"reserved filename");
    migrate(&mut db, &path, APPLICATION_ID).unwrap();
    assert_eq!(backups(&dir).len(), 2);
    assert_eq!(fs::read(&files[0]).unwrap(), b"reserved filename");
}

#[cfg(unix)]
#[test]
fn directory_sync_failures_after_publication_block_the_upgrade() {
    for fail_on_parent in [false, true] {
        let (dir, path, mut db) = version_one();
        db.execute("INSERT INTO processing_task(state) VALUES ('running')", [])
            .unwrap();
        let error = migrate_with_publisher(&mut db, &path, APPLICATION_ID, |temp, destination| {
            publication::publish_and_sync(temp, destination, |directory| {
                assert!(destination.is_file());
                if (directory == dir.path()) == fail_on_parent {
                    return Err(std::io::Error::other("injected directory sync failure"));
                }
                fs::File::open(directory)?.sync_all()
            })
        })
        .unwrap_err();
        assert!(matches!(error, StorageError::Io(_)), "{error}");
        assert_version_one(&db);
        let files = backups(&dir);
        assert_eq!(files.len(), 1);
        assert_version_one(&Connection::open(&files[0]).unwrap());
        migrate(&mut db, &path, APPLICATION_ID).unwrap();
        assert_eq!(backups(&dir).len(), 2);
    }
}

#[test]
fn write_transaction_rechecks_version_after_successful_preflight() {
    let (dir, path, mut db) = version_one();
    preflight(&path, APPLICATION_ID).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    db.pragma_update(None, "user_version", 500).unwrap();
    let before = fs::read(&path).unwrap();
    let wal = dir.path().join("tasks.sqlite3-wal");
    let frames = fs::read(&wal).unwrap();
    assert!(matches!(
        migrate(&mut db, &path, APPLICATION_ID),
        Err(StorageError::NewerVersion(500))
    ));
    assert!(!dir.path().join("backups").exists());
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        500
    );
    drop(db);
    assert!(fs::read(path).unwrap() == before);
    assert!(fs::read(wal).unwrap() == frames);
}
