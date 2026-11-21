use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tempfile::tempdir;

fn cli(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_videosift"));
    command.arg("--data-dir").arg(dir);
    command
}

fn events(output: Output, code: i32) -> Vec<Value> {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn success(dir: &Path, args: &[&str]) -> Vec<Value> {
    events(cli(dir).args(args).output().unwrap(), 0)
}

#[test]
fn cold_and_warm_runs_publish_the_same_result_for_distinct_tasks() {
    let dir = tempdir().unwrap();
    let cold = success(dir.path(), &["run"]);
    let warm = success(dir.path(), &["run"]);
    for (run, hit) in [(&cold, false), (&warm, true)] {
        assert_eq!(run.len(), 4);
        assert_eq!(run[0]["state"], "queued");
        assert_eq!(run[1]["event"], "worker_checkpoint");
        assert_eq!(run[1]["cache_hit"], hit);
        assert_eq!(run[2]["event"], "provider_completed");
        assert_eq!(run[2]["cache_hit"], hit);
        assert_eq!(run[3]["state"], "succeeded");
        assert_eq!(run[3]["output_ref"].as_str().unwrap().len(), 64);
        let id = run[3]["task_id"].as_str().unwrap();
        assert_eq!(success(dir.path(), &["status", id]), run[3..]);
    }
    assert_ne!(cold[3]["task_id"], warm[3]["task_id"]);
    assert_eq!(cold[3]["output_ref"], warm[3]["output_ref"]);
}

#[test]
fn damaged_outputs_are_regenerated_before_a_new_task_reuses_them() {
    let dir = tempdir().unwrap();
    let cold = success(dir.path(), &["run"]);
    for file in std::fs::read_dir(dir.path().join("cache")).unwrap() {
        std::fs::write(file.unwrap().path(), b"damaged").unwrap();
    }
    let repaired = success(dir.path(), &["run"]);
    assert_eq!(repaired[1]["cache_hit"], false);
    assert_eq!(repaired[2]["cache_hit"], false);
    assert_eq!(repaired[3]["output_ref"], cold[3]["output_ref"]);
    let warm = success(dir.path(), &["run"]);
    assert_eq!(warm[1]["cache_hit"], true);
    assert_eq!(warm[2]["cache_hit"], true);
}

#[test]
fn cancellation_settles_without_publishing_and_rejects_resume() {
    let dir = tempdir().unwrap();
    let mut child = cli(dir.path())
        .args(["run", "--interactive"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"cancel\r\n")
        .unwrap();
    let run = events(child.wait_with_output().unwrap(), 3);
    assert_eq!(run.len(), 3);
    let task = &run[2];
    assert_eq!(task["state"], "cancelled");
    assert!(task["checkpoint"].is_string());
    assert!(task["output_ref"].is_null());
    let id = task["task_id"].as_str().unwrap();
    assert_eq!(success(dir.path(), &["cancel", id])[0], *task);
    let denied = cli(dir.path()).args(["resume", id]).output().unwrap();
    assert_eq!(denied.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&denied.stderr).unwrap();
    assert_eq!(error["task_id"], id);
    assert_eq!(error["code"], "validation");
    assert_eq!(success(dir.path(), &["status", id])[0], *task);
}

struct Foreground(Child);

impl Drop for Foreground {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn killed_owner_requires_explicit_resume_and_reuses_its_verified_worker_checkpoint() {
    let dir = tempdir().unwrap();
    let mut owner = Foreground(
        cli(dir.path())
            .args(["run", "--interactive"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let out = owner.0.stdout.take().unwrap();
    let (send, recv) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            send.send(serde_json::from_str::<Value>(&line).unwrap())
                .unwrap();
        }
    });
    let next = || recv.recv_timeout(Duration::from_secs(15)).unwrap();
    let queued = next();
    let id = queued["task_id"].as_str().unwrap();
    assert_eq!(next()["event"], "worker_checkpoint");
    let busy = cli(dir.path()).args(["status", id]).output().unwrap();
    assert_eq!(busy.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&busy.stderr).unwrap()["code"],
        "storage"
    );
    drop(owner);
    let stopped = success(dir.path(), &["status", id]);
    assert_eq!(stopped[0]["state"], "interrupted");
    assert!(stopped[0]["checkpoint"].is_string());
    assert!(stopped[0]["output_ref"].is_null());
    assert_eq!(success(dir.path(), &["status", id]), stopped);
    let resumed = success(dir.path(), &["resume", id]);
    assert_eq!(resumed[1]["cache_hit"], true);
    assert_eq!(resumed[2]["cache_hit"], false);
    assert_eq!(resumed[3]["state"], "succeeded");
    assert_eq!(resumed[3]["task_id"], id);
    assert_eq!(resumed[3]["checkpoint"], stopped[0]["checkpoint"]);
}

#[test]
fn invalid_input_and_eof_leave_no_unattended_provider() {
    for (input, code) in [("invalid\n", 2), ("", 3), ("continue\n", 0)] {
        let dir = tempdir().unwrap();
        let mut child = cli(dir.path())
            .args(["run", "--interactive"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let run = events(child.wait_with_output().unwrap(), code);
        let last = run.last().unwrap();
        assert_eq!(
            last["state"],
            if code == 0 { "succeeded" } else { "cancelled" }
        );
        assert_eq!(last["output_ref"].is_null(), code != 0);
    }
}

#[test]
fn mock_commands_reject_an_existing_core_database_before_recovery() {
    use videosift_core::storage::TaskStore;
    let dir = tempdir().unwrap();
    let mut core = TaskStore::open(dir.path()).unwrap();
    let task = core.submit().unwrap();
    let execution = core.dispatch(task).unwrap();
    core.save_checkpoint(&execution, "original-checkpoint")
        .unwrap();
    let completed = core.submit().unwrap();
    let finished = core.dispatch(completed).unwrap();
    core.complete(&finished, "original-final", "original-output")
        .unwrap();
    drop(core);
    let path = dir.path().join("tasks.sqlite3");
    let before = std::fs::read(&path).unwrap();
    let id = task.to_string();
    for args in [
        vec!["status", &id],
        vec!["resume", &id],
        vec!["cancel", &id],
        vec!["run"],
    ] {
        let rejected = cli(dir.path()).args(args).output().unwrap();
        assert_eq!(rejected.status.code(), Some(1));
        assert!(rejected.stdout.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&rejected.stderr).unwrap()["code"],
            "storage"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!dir.path().join("cache").exists());
    }
}

#[test]
fn half_line_times_out_and_releases_store_while_pipe_is_still_open() {
    let dir = tempdir().unwrap();
    let mut child = Foreground(
        cli(dir.path())
            .args(["run", "--interactive"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut pipe = child.0.stdin.take().unwrap();
    pipe.write_all(b"cont").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert_eq!(status.code(), Some(4));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "CLI still owns the store"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let stdout = child.0.stdout.take().unwrap();
    let last = BufReader::new(stdout).lines().last().unwrap().unwrap();
    let terminal: Value = serde_json::from_str(&last).unwrap();
    let id = terminal["task_id"].as_str().unwrap();
    assert_eq!(terminal["state"], "cancelled");
    assert!(terminal["checkpoint"].is_string());
    assert!(terminal["output_ref"].is_null());
    let err: Value = serde_json::from_reader(child.0.stderr.take().unwrap()).unwrap();
    assert_eq!(err["code"], "cancellation");
    assert_eq!(err["task_id"], id);
    assert_eq!(success(dir.path(), &["status", id])[0], terminal);
    drop(pipe);
}

#[cfg(windows)]
#[test]
fn locked_old_files_emit_warnings_and_repaired_stages_complete() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempdir().unwrap();
    let cold = success(dir.path(), &["run"]);
    let cache = dir.path().join("cache");
    let mut held = Vec::new();
    for entry in std::fs::read_dir(&cache).unwrap() {
        let path = entry.unwrap().path();
        std::fs::write(&path, b"damaged").unwrap();
        // Share read/write but deny delete until the handle closes.
        held.push(
            std::fs::File::options()
                .read(true)
                .share_mode(3)
                .open(path)
                .unwrap(),
        );
    }
    let repaired = cli(dir.path()).arg("run").output().unwrap();
    let warnings: Vec<Value> = serde_json::Deserializer::from_slice(&repaired.stderr)
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(warnings.len(), 2);
    for (warning, stage) in warnings.iter().zip(["worker", "provider"]) {
        assert_eq!(warning["event"], "warning");
        assert_eq!(warning["stage"], stage);
    }
    let run = events(repaired, 0);
    assert_eq!(run[3]["state"], "succeeded");
    assert_eq!(run[3]["output_ref"], cold[3]["output_ref"]);
    assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 4);
    drop(held);
    let warm = success(dir.path(), &["run"]);
    assert_eq!(warm[1]["cache_hit"], true);
    assert_eq!(warm[2]["cache_hit"], true);
    assert_eq!(std::fs::read_dir(cache).unwrap().count(), 2);
}

#[test]
fn existing_unmarked_empty_database_is_not_claimed_as_mock() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("tasks.sqlite3");
    std::fs::File::create(&path).unwrap();
    let rejected = cli(dir.path()).arg("run").output().unwrap();
    assert_eq!(rejected.status.code(), Some(1));
    assert!(std::fs::read(path).unwrap().is_empty());
}
