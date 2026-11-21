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
