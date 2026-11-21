mod identity;

#[cfg(test)]
mod tests;

pub use identity::{ArtifactIdentity, CacheKey, Stage, StageIdentity};

use std::fs;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{StorageError, TaskStore, publication, state};

pub struct CachedArtifact {
    pub identity: ArtifactIdentity,
    pub bytes: Vec<u8>,
}

fn object_path(directory: &Path, id: [u8; 32]) -> PathBuf {
    directory.join(format!("{}.blob", blake3::Hash::from_bytes(id).to_hex()))
}

// Never follow a cache directory link during cleanup.
fn directory_exists(directory: &Path) -> Result<bool, StorageError> {
    match fs::symlink_metadata(directory) {
        Ok(meta) if meta.is_dir() && !is_link(&meta) => Ok(true),
        Ok(_) => Err(StorageError::InvalidDirectory),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

#[cfg(windows)]
fn is_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

#[cfg(unix)]
fn is_link(metadata: &fs::Metadata) -> bool {
    metadata.is_symlink()
}

/// Called under the SQLite writer lock, so another connection cannot be staging a live file.
pub(super) fn recover(db: &Connection, directory: &Path) -> Result<(), StorageError> {
    if !directory_exists(directory)? {
        return Ok(());
    }
    let mut indexed = db.prepare("SELECT EXISTS(SELECT 1 FROM cache_entry WHERE object_id=?1)")?;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let staged = name.starts_with("stage-") && name.ends_with(".tmp");
        let orphan = if let Some(id) = name
            .strip_suffix(".blob")
            .and_then(|s| blake3::Hash::from_hex(s).ok())
        {
            name == format!("{}.blob", id.to_hex())
                && !indexed.query_row([id.as_bytes()], |row| row.get::<_, bool>(0))?
        } else {
            false
        };
        if staged || orphan {
            remove_file_if_present(&entry.path())?;
        }
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

struct IndexedArtifact {
    id: [u8; 32],
    digest: blake3::Hash,
    length: u64,
}

fn indexed_artifact(
    db: &Connection,
    key: &CacheKey,
) -> Result<Option<IndexedArtifact>, StorageError> {
    Ok(db
        .query_row(
            "SELECT object_id, digest, byte_length FROM cache_entry WHERE cache_key=?1",
            [key.as_str()],
            |row| {
                Ok(IndexedArtifact {
                    id: row.get(0)?,
                    digest: blake3::Hash::from_bytes(row.get(1)?),
                    length: row.get::<_, i64>(2)? as u64,
                })
            },
        )
        .optional()?)
}

impl IndexedArtifact {
    fn read_verified(&self, directory: &Path) -> Result<Vec<u8>, StorageError> {
        if !directory_exists(directory)? {
            return Err(StorageError::CacheIntegrity);
        }
        let path = object_path(directory, self.id);
        let file_error = |e: io::Error| {
            if e.kind() == io::ErrorKind::NotFound {
                StorageError::CacheIntegrity
            } else {
                e.into()
            }
        };
        let metadata = fs::symlink_metadata(&path).map_err(file_error)?;
        if !metadata.is_file() || is_link(&metadata) || metadata.len() != self.length {
            return Err(StorageError::CacheIntegrity);
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(file_error)?
            .take(self.length + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != self.length || blake3::hash(&bytes) != self.digest {
            return Err(StorageError::CacheIntegrity);
        }
        Ok(bytes)
    }
}

fn prepare_repair(
    db: &Connection,
    directory: &Path,
    key: &CacheKey,
    old: Option<&IndexedArtifact>,
    digest: blake3::Hash,
) -> Result<(), StorageError> {
    let old = old.ok_or(StorageError::CacheRepairNotNeeded)?;
    match old.read_verified(directory) {
        Err(StorageError::CacheIntegrity) => {}
        Ok(_) => return Err(StorageError::CacheRepairNotNeeded),
        Err(error) => return Err(error),
    }

    if old.digest != digest {
        let executing: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM processing_task t JOIN cache_reference r
             ON t.id=r.task_id WHERE r.cache_key=?1 AND t.execution IS NOT NULL)",
            [key.as_str()],
            |row| row.get(0),
        )?;
        if executing {
            return Err(StorageError::CacheRepairBusy);
        }
        // Checkpoints are opaque. Restart affected unfinished tasks from their first step.
        db.execute(
            "UPDATE processing_task SET checkpoint=NULL WHERE id IN
             (SELECT task_id FROM cache_reference WHERE cache_key=?1)
             AND state NOT IN ('succeeded','failed','cancelled')",
            [key.as_str()],
        )?;
    }
    Ok(())
}

fn stamp(db: &Connection) -> rusqlite::Result<i64> {
    db.query_row(
        "SELECT coalesce(max(last_used), 0) + 1 FROM cache_entry",
        [],
        |row| row.get(0),
    )
}

fn pin(db: &Connection, key: &CacheKey, task: Option<i64>) -> Result<(), StorageError> {
    if let Some(task) = task {
        state(db, task)?;
        db.execute(
            "INSERT OR IGNORE INTO cache_reference(cache_key, task_id) VALUES (?1, ?2)",
            params![key.as_str(), task],
        )?;
    }
    Ok(())
}

fn artifact(key: &CacheKey, hash: blake3::Hash) -> ArtifactIdentity {
    ArtifactIdentity {
        key: key.clone(),
        digest: hash.to_hex().to_string(),
    }
}

impl TaskStore {
    /// Publish a new immutable generation. A failed SQL commit leaves the previous one readable.
    pub fn cache_put(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
        task: Option<i64>,
    ) -> Result<ArtifactIdentity, StorageError> {
        self.publish_cache(key, bytes, task, false)
    }

    /// Rebuild an indexed artifact after verifying the previous generation is damaged.
    /// A changed digest requires settled executions and invalidates referencing checkpoints
    /// in the publication transaction. Task references remain attached to the stage key.
    pub fn repair_cache(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
    ) -> Result<ArtifactIdentity, StorageError> {
        self.publish_cache(key, bytes, None, true)
    }

    fn publish_cache(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
        task: Option<i64>,
        repair: bool,
    ) -> Result<ArtifactIdentity, StorageError> {
        let directory = self.path.with_file_name("cache");
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let digest = blake3::hash(bytes);
        let old = indexed_artifact(&tx, key)?;
        if repair {
            prepare_repair(&tx, &directory, key, old.as_ref(), digest)?;
        } else if old.as_ref().is_some_and(|entry| entry.digest != digest) {
            let in_use: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cache_reference r JOIN processing_task t ON t.id=r.task_id
                 WHERE r.cache_key=?1 AND t.state NOT IN ('succeeded','failed','cancelled'))",
                [key.as_str()],
                |row| row.get(0),
            )?;
            if in_use {
                return Err(StorageError::CacheInUse);
            }
        }
        pin(&tx, key, task)?;
        if !directory_exists(&directory)? {
            fs::create_dir(&directory)?;
        }
        let mut temp = tempfile::Builder::new()
            .prefix("stage-")
            .suffix(".tmp")
            .tempfile_in(&directory)?;
        temp.write_all(bytes)?;
        temp.as_file().sync_all()?;
        temp.rewind()?;
        let staged = blake3::Hasher::new()
            .update_reader(temp.as_file_mut())?
            .finalize();
        if temp.as_file().metadata()?.len() != bytes.len() as u64 || digest != staged {
            return Err(StorageError::CacheIntegrity);
        }
        let id: [u8; 32] = tx.query_row("SELECT randomblob(32)", [], |r| r.get(0))?;
        publication::publish(temp, &object_path(&directory, id))?;
        tx.execute(
            "INSERT INTO cache_entry(cache_key, object_id, digest, byte_length, last_used)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(cache_key) DO UPDATE SET
             object_id=excluded.object_id, digest=excluded.digest,
             byte_length=excluded.byte_length, last_used=excluded.last_used",
            params![
                key.as_str(),
                id,
                digest.as_bytes(),
                bytes.len() as i64,
                stamp(&tx)?
            ],
        )?;
        tx.commit()?;
        // IDs are unique per publication. Recovery may have collected this generation already.
        if let Some(old) = old {
            remove_file_if_present(&object_path(&directory, old.id))
                .map_err(StorageError::CacheCleanup)?;
        }
        Ok(artifact(key, digest))
    }

    /// None is a cold or invalidated key. An indexed but damaged artifact requires regeneration.
    pub fn cache_get(
        &mut self,
        key: &CacheKey,
        task: Option<i64>,
    ) -> Result<Option<CachedArtifact>, StorageError> {
        let directory = self.path.with_file_name("cache");
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(entry) = indexed_artifact(&tx, key)? else {
            return Ok(None);
        };
        let bytes = entry.read_verified(&directory)?;
        pin(&tx, key, task)?;
        tx.execute(
            "UPDATE cache_entry SET last_used=?1 WHERE cache_key=?2",
            params![stamp(&tx)?, key.as_str()],
        )?;
        tx.commit()?;
        Ok(Some(CachedArtifact {
            identity: artifact(key, entry.digest),
            bytes,
        }))
    }

    /// Evict least-recently-used unprotected entries. Return remaining indexed payload bytes,
    /// which can exceed the budget when unfinished tasks still reference artifacts.
    pub fn trim_cache(&mut self, budget: u64) -> Result<u64, StorageError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut size = tx.query_row(
            "SELECT coalesce(sum(byte_length), 0) FROM cache_entry",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64;
        tx.execute(
            "DELETE FROM cache_reference WHERE task_id IN
            (SELECT id FROM processing_task WHERE state IN ('succeeded', 'failed', 'cancelled'))",
            [],
        )?;
        while size > budget {
            let victim = tx
                .query_row(
                    "DELETE FROM cache_entry WHERE cache_key=(
                SELECT e.cache_key FROM cache_entry e WHERE NOT EXISTS (
                SELECT 1 FROM cache_reference r WHERE r.cache_key=e.cache_key)
                ORDER BY e.last_used LIMIT 1) RETURNING byte_length",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?;
            let Some(length) = victim else { break };
            size -= length as u64;
        }
        tx.commit()?;
        self.cleanup_cache()?;
        Ok(size)
    }

    /// Retry orphan cleanup under the SQLite writer lock.
    pub fn cleanup_cache(&mut self) -> Result<(), StorageError> {
        let directory = self.path.with_file_name("cache");
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        recover(&tx, &directory)?;
        tx.commit()?;
        self.startup_cache_cleanup_error = None;
        Ok(())
    }
}
