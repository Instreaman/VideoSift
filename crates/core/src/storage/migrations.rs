use std::path::Path;

use rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE;
use rusqlite::{Connection, MAIN_DB, OpenFlags, TransactionBehavior, backup::NO_PROGRESS};

use super::StorageError;

mod publication;

#[cfg(test)]
mod tests;

const APPLICATION_ID: i32 = 0x56534654; // VSFT
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE processing_task (
        id INTEGER PRIMARY KEY,
        state TEXT NOT NULL
    ) STRICT;",
    "ALTER TABLE processing_task ADD COLUMN execution BLOB;
     ALTER TABLE processing_task ADD COLUMN checkpoint TEXT;
     ALTER TABLE processing_task ADD COLUMN output_ref TEXT;
     CREATE UNIQUE INDEX active_execution ON processing_task(execution)
         WHERE execution IS NOT NULL;",
];

fn checked_version(connection: &Connection) -> Result<i32, StorageError> {
    let version: i32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > MIGRATIONS.len() as i32 {
        return Err(StorageError::NewerVersion(version));
    }
    let application: i32 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if version == 0 && application == 0 {
        let empty: bool =
            connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema)", [], |r| {
                r.get(0)
            })?;
        if !empty {
            return Err(StorageError::ForeignDatabase);
        }
    } else if version <= 0 || application != APPLICATION_ID {
        return Err(StorageError::ForeignDatabase);
    }
    Ok(version)
}

/// Read the committed version, including any WAL frames, without opening a writer.
pub(super) fn preflight(path: &Path) -> Result<(), StorageError> {
    let mut reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tx = reader.transaction_with_behavior(TransactionBehavior::Deferred)?;
    match checked_version(&tx) {
        // SQLite cannot read a hot rollback journal without replaying it. Under
        // the core owner lock, recover a private copy to identify the committed
        // version before permitting SQLite to touch the original.
        Err(StorageError::Database(rusqlite::Error::SqliteFailure(code, _)))
            if code.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK =>
        {
            let dir = tempfile::tempdir_in(path.parent().ok_or(StorageError::InvalidDirectory)?)?;
            let copy = dir.path().join("tasks.sqlite3");
            std::fs::copy(path, &copy)?;
            std::fs::copy(
                path.with_file_name("tasks.sqlite3-journal"),
                dir.path().join("tasks.sqlite3-journal"),
            )?;
            let snapshot = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
            checked_version(&snapshot)?;
        }
        result => {
            result?;
        }
    }
    Ok(())
}

pub(super) fn migrate(connection: &mut Connection, path: &Path) -> Result<(), StorageError> {
    migrate_with_publisher(connection, path, publication::publish)
}

fn migrate_with_publisher(
    connection: &mut Connection,
    path: &Path,
    publish: impl FnOnce(tempfile::NamedTempFile, &Path) -> std::io::Result<()>,
) -> Result<(), StorageError> {
    // If the locked recheck rejects a newer WAL, closing this connection must
    // not checkpoint it either. Restore SQLite's normal policy once admitted.
    connection.set_db_config(SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = checked_version(&tx)?;
    tx.set_db_config(SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, false)?;
    if version > 0 && version < MIGRATIONS.len() as i32 {
        // A second connection reads the committed snapshot while this transaction excludes writers.
        let temp = stage_backup(path, version)?;
        let destination = temp.path().with_extension("backup.sqlite3");
        publish(temp, &destination)?;
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as i32 + 1)?;
    }
    if version == 0 {
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
    }
    tx.commit()?;
    Ok(())
}

fn stage_backup(path: &Path, version: i32) -> Result<tempfile::NamedTempFile, StorageError> {
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let directory = path
        .parent()
        .ok_or(StorageError::InvalidDirectory)?
        .join("backups");
    std::fs::create_dir_all(&directory)?;
    let temp = tempfile::Builder::new()
        .prefix(&format!("tasks-v{version}-"))
        .suffix(".tmp")
        .tempfile_in(directory)?;
    source.backup(MAIN_DB, temp.path(), NO_PROGRESS)?;
    temp.as_file().sync_all()?;
    Ok(temp)
}
