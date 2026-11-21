use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use rusqlite::Connection;
use tempfile::tempdir;
use videosift_core::storage::{
    StorageError, TaskStore,
    cache::{Stage, StageIdentity},
};

fn stage(stage: Stage) -> StageIdentity<'static> {
    StageIdentity {
        source_id: "fixture",
        content_version: "1",
        content_fingerprint: "片段",
        part_id: "p1",
        stage,
        model: None,
        model_version: None,
        algorithm_version: "1",
        prompt_version: None,
        schema_version: "1",
        dependencies: BTreeMap::new(),
    }
}

fn objects(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory.join("cache"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "blob"))
        .collect()
}

#[test]
fn canonical_key_matches_fixed_utf8_fixture_and_rejects_invalid_parameters() {
    let input = stage(Stage::Asr);
    let golden = r#"{"algorithm_version":"1","content_fingerprint":"片段","content_version":"1","dependencies":{},"inference_params":{"a":{"x":2,"y":1},"z":[0.5]},"model":null,"model_version":null,"part_id":"p1","prompt_version":null,"schema_version":"1","source_id":"fixture","stage":"asr"}"#;
    let key = input.key(r#"{ "z":[0.5], "a":{"y":1,"x":2} }"#).unwrap();
    assert_eq!(
        key.as_str(),
        blake3::hash(golden.as_bytes()).to_hex().as_str()
    );
    for invalid in [
        r#"{"x":NaN}"#,
        r#"{"x":Infinity}"#,
        r#"{"x":1e999}"#,
        "null",
        "[]",
    ] {
        assert!(matches!(
            input.key(invalid),
            Err(StorageError::CacheIdentity(_))
        ));
    }
    assert_ne!(
        input.key(r#"{"seed":18446744073709551616}"#).unwrap(),
        input.key(r#"{"seed":18446744073709551617}"#).unwrap(),
    );
}

#[test]
fn changed_inputs_invalidate_only_consumers_and_their_descendants() {
    let dir = tempdir().unwrap();
    let mut store = TaskStore::open(dir.path()).unwrap();
    let mut asr = stage(Stage::Asr);
    let ocr = stage(Stage::Ocr).key("{}").unwrap();
    let first = asr.key("{}").unwrap();
    assert!(store.cache_get(&first, None).unwrap().is_none());
    let transcript = store.cache_put(&first, b"words", None).unwrap();
    store.cache_put(&ocr, b"text", None).unwrap();
    let mut knowledge = stage(Stage::Knowledge);
    knowledge.dependencies.insert("asr", transcript);
    let original = knowledge.key("{}").unwrap();
    store.cache_put(&original, b"evidence", None).unwrap();
    asr.model = Some("mock");
    asr.model_version = Some("2");
    let next = asr.key("{}").unwrap();
    assert!(store.cache_get(&next, None).unwrap().is_none());
    let updated = store.cache_put(&next, b"words", None).unwrap();
    knowledge.dependencies.insert("asr", updated);
    assert!(
        store
            .cache_get(&knowledge.key("{}").unwrap(), None)
            .unwrap()
            .is_none()
    );
    let same_recipe = store.cache_put(&first, b"new words", None).unwrap();
    knowledge.dependencies.insert("asr", same_recipe);
    assert_ne!(knowledge.key("{}").unwrap(), original);
    assert_eq!(store.cache_get(&ocr, None).unwrap().unwrap().bytes, b"text");
}

#[test]
fn missing_truncated_and_same_length_corrupt_content_can_be_rebuilt() {
    let dir = tempdir().unwrap();
    let key = stage(Stage::Metadata).key("{}").unwrap();
    let mut store = TaskStore::open(dir.path()).unwrap();
    for replacement in [None, Some(&b"x"[..]), Some(&b"wrong"[..])] {
        let expected = store.cache_put(&key, b"right", None).unwrap();
        let path = objects(dir.path()).pop().unwrap();
        if let Some(bytes) = replacement {
            fs::write(path, bytes).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        drop(store);
        store = TaskStore::open(dir.path()).unwrap();
        assert!(matches!(
            store.cache_get(&key, None),
            Err(StorageError::CacheIntegrity)
        ));
        assert_eq!(store.cache_put(&key, b"right", None).unwrap(), expected);
        let hit = store.cache_get(&key, None).unwrap().unwrap();
        assert_eq!(hit.bytes, b"right");
        assert_eq!(hit.identity, expected);
    }
}

#[test]
fn failed_index_publication_preserves_old_generation_and_restart_collects_orphans() {
    let dir = tempdir().unwrap();
    let key = stage(Stage::Subtitle).key("{}").unwrap();
    let mut store = TaskStore::open(dir.path()).unwrap();
    store.cache_put(&key, b"old", None).unwrap();
    let db = Connection::open(dir.path().join("tasks.sqlite3")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject BEFORE UPDATE OF object_id ON cache_entry
        BEGIN SELECT RAISE(ABORT, 'fault injection'); END",
    )
    .unwrap();
    assert!(matches!(
        store.cache_put(&key, b"new", None),
        Err(StorageError::Database(_))
    ));
    assert_eq!(objects(dir.path()).len(), 2);
    assert_eq!(store.cache_get(&key, None).unwrap().unwrap().bytes, b"old");
    let cache = dir.path().join("cache");
    fs::write(cache.join("stage-aborted.tmp"), b"incomplete").unwrap();
    fs::write(cache.join("other.tmp"), b"preserve").unwrap();
    drop(store);
    let mut store = TaskStore::open(dir.path()).unwrap();
    assert_eq!(objects(dir.path()).len(), 1);
    assert!(!cache.join("stage-aborted.tmp").exists());
    assert!(cache.join("other.tmp").exists());
    db.execute_batch("DROP TRIGGER reject").unwrap();
    store.cache_put(&key, b"new", None).unwrap();
    assert_eq!(store.cache_get(&key, None).unwrap().unwrap().bytes, b"new");
}

#[test]
fn budget_respects_hit_order_and_unfinished_references_across_restart() {
    let dir = tempdir().unwrap();
    let mut store = TaskStore::open(dir.path()).unwrap();
    let task = store.submit().unwrap();
    store.dispatch(task).unwrap();
    let keys = [Stage::Asr, Stage::Ocr, Stage::Subtitle].map(|s| stage(s).key("{}").unwrap());
    for key in &keys {
        store.cache_put(key, b"abc", None).unwrap();
    }
    let mut reader = store.writer().unwrap();
    reader.cache_get(&keys[0], None).unwrap();
    reader.cache_get(&keys[2], Some(task)).unwrap();
    assert_eq!(store.trim_cache(6).unwrap(), 6);
    assert!(reader.cache_get(&keys[1], None).unwrap().is_none());
    drop(reader);
    drop(store);
    let mut store = TaskStore::open(dir.path()).unwrap();
    assert_eq!(store.trim_cache(0).unwrap(), 3);
    assert!(matches!(
        store.cache_put(&keys[2], b"xyz", None),
        Err(StorageError::CacheInUse)
    ));
    assert_eq!(
        store.cache_get(&keys[2], None).unwrap().unwrap().bytes,
        b"abc"
    );
    store.resume(task).unwrap();
    let ticket = store.dispatch(task).unwrap();
    store.complete(&ticket, "step", "result").unwrap();
    assert_eq!(store.trim_cache(0).unwrap(), 0);
    assert!(objects(dir.path()).is_empty());
}

#[test]
fn staging_io_error_rolls_back_the_index_and_task_pin() {
    let dir = tempdir().unwrap();
    let mut store = TaskStore::open(dir.path()).unwrap();
    let task = store.submit().unwrap();
    let key = stage(Stage::Keyframe).key("{}").unwrap();
    let path = dir.path().join("cache");
    fs::write(&path, b"existing file").unwrap();
    assert!(matches!(
        store.cache_put(&key, b"frame", Some(task)),
        Err(StorageError::InvalidDirectory)
    ));
    assert_eq!(fs::read(&path).unwrap(), b"existing file");
    assert!(store.cache_get(&key, None).unwrap().is_none());
    fs::remove_file(path).unwrap();
    store.cache_put(&key, b"retry", None).unwrap();
    assert_eq!(store.trim_cache(0).unwrap(), 0);
}
