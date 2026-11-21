use super::{CacheKey, StorageError, TaskStore};

#[test]
fn disk_full_during_index_update_keeps_last_committed_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = TaskStore::open(dir.path()).unwrap();
    let key = CacheKey(blake3::hash(b"synthetic recipe").to_hex().to_string());
    store.cache_put(&key, b"first", None).unwrap();
    store
        .connection
        .execute_batch(
            "CREATE TABLE pressure(payload BLOB);
         CREATE TRIGGER fill AFTER UPDATE OF object_id ON cache_entry
         BEGIN INSERT INTO pressure VALUES(zeroblob(1048576)); END;",
        )
        .unwrap();
    let pages: i64 = store
        .connection
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    store
        .connection
        .pragma_update(None, "max_page_count", pages)
        .unwrap();
    let error = store.cache_put(&key, b"second", None).unwrap_err();
    assert!(
        matches!(error, StorageError::Database(ref e) if e.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull))
    );
    drop(store);
    let mut store = TaskStore::open(dir.path()).unwrap();
    assert_eq!(
        store.cache_get(&key, None).unwrap().unwrap().bytes,
        b"first"
    );
    assert_eq!(
        std::fs::read_dir(dir.path().join("cache")).unwrap().count(),
        1
    );
    store.connection.execute_batch("DROP TRIGGER fill").unwrap();
    store.cache_put(&key, b"second", None).unwrap();
    assert_eq!(
        store.cache_get(&key, None).unwrap().unwrap().bytes,
        b"second"
    );
}
