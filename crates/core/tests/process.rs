use std::path::PathBuf;
use std::time::Duration;

use tokio::time::{sleep, timeout};
use videosift_core::process::{
    CHUNK_BYTES, Limits, ProcessError, ProcessSpec, ProcessSupervisor, Stream, Termination,
};
use videosift_core::scheduler::{Execution, SchedulerError};
use videosift_core::task::TaskState;

fn spec(mode: &str) -> ProcessSpec {
    ProcessSpec {
        program: PathBuf::from(env!("CARGO_BIN_EXE_process-test-helper")),
        args: vec![mode.into()],
    }
}

fn limits() -> Limits {
    Limits {
        pipe_chunks: 2,
        run_timeout: Duration::from_secs(10),
        cancel_grace: Duration::from_millis(300),
        cleanup_timeout: Duration::from_secs(5),
    }
}

async fn ready(supervisor: &mut ProcessSupervisor, execution: Execution) -> String {
    timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        while !bytes.contains(&b'\n') {
            let chunk = supervisor.recv_output(execution).await.unwrap().unwrap();
            assert_eq!(chunk.stream, Stream::Stdout);
            bytes.extend(chunk.bytes);
        }
        String::from_utf8(bytes).unwrap()
    })
    .await
    .expect("fixture did not start")
}

async fn settle(
    supervisor: &mut ProcessSupervisor,
    execution: Execution,
) -> videosift_core::process::ProcessReport {
    timeout(Duration::from_secs(15), supervisor.settle(execution))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn cooperative_cancel_holds_slot_until_confirmed_cleanup() {
    let mut supervisor = ProcessSupervisor::new(2, 1).unwrap();
    let task = supervisor.submit().unwrap();
    supervisor.submit().unwrap();
    let execution = supervisor
        .dispatch(spec("cooperate"), limits())
        .unwrap()
        .unwrap();
    assert_eq!(ready(&mut supervisor, execution).await, "ready\n");
    assert_eq!(supervisor.cancel(task).unwrap(), TaskState::CancelRequested);
    assert_eq!(supervisor.cancel(task).unwrap(), TaskState::CancelRequested);
    assert_eq!(supervisor.in_flight(), 1);
    assert!(
        supervisor
            .dispatch(spec("success"), limits())
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        supervisor.try_send_input(execution, b"late"),
        Err(ProcessError::InputClosed)
    ));
    let report = settle(&mut supervisor, execution).await;
    assert_eq!(
        report.termination,
        Termination::Cancelled {
            grace_expired: false
        }
    );
    assert!(report.cleanup_complete);
    assert_eq!(
        supervisor.snapshot(task).unwrap().state,
        TaskState::Cancelled
    );
    assert_eq!(supervisor.in_flight(), 0);
    let next = supervisor
        .dispatch(spec("success"), limits())
        .unwrap()
        .unwrap();
    settle(&mut supervisor, next).await;
}

#[tokio::test]
async fn grace_expiry_kills_parent_and_descendant() {
    let mut supervisor = ProcessSupervisor::new(1, 1).unwrap();
    let task = supervisor.submit().unwrap();
    let execution = supervisor
        .dispatch(spec("tree"), limits())
        .unwrap()
        .unwrap();
    let message = ready(&mut supervisor, execution).await;
    let pid: u32 = message
        .trim()
        .strip_prefix("descendant=")
        .unwrap()
        .parse()
        .unwrap();
    assert!(process_alive(pid));
    supervisor.cancel(task).unwrap();
    let report = settle(&mut supervisor, execution).await;
    assert_eq!(
        report.termination,
        Termination::Cancelled {
            grace_expired: true
        }
    );
    assert_eq!(supervisor.in_flight(), 0);
    assert_dead(pid).await;
}

#[tokio::test]
async fn parent_exit_also_cleans_up_descendants() {
    let mut supervisor = ProcessSupervisor::new(1, 1).unwrap();
    let task = supervisor.submit().unwrap();
    let execution = supervisor
        .dispatch(spec("orphan"), limits())
        .unwrap()
        .unwrap();
    let message = ready(&mut supervisor, execution).await;
    let pid: u32 = message
        .trim()
        .strip_prefix("descendant=")
        .unwrap()
        .parse()
        .unwrap();
    let report = settle(&mut supervisor, execution).await;
    assert!(report.cleanup_complete);
    assert_eq!(report.termination, Termination::Exited { success: true });
    assert!(supervisor.snapshot(task).unwrap().worker_completed);
    assert_dead(pid).await;
}

#[tokio::test]
async fn unread_output_and_blocked_stdin_do_not_block_cancellation() {
    let mut supervisor = ProcessSupervisor::new(1, 1).unwrap();
    let task = supervisor.submit().unwrap();
    let execution = supervisor
        .dispatch(spec("flood"), limits())
        .unwrap()
        .unwrap();
    // The current-thread runtime has not polled the writer: channel capacity is exact.
    let chunk = [b'i'; CHUNK_BYTES];
    supervisor.try_send_input(execution, &chunk).unwrap();
    supervisor.try_send_input(execution, &chunk).unwrap();
    assert!(matches!(
        supervisor.try_send_input(execution, &chunk),
        Err(ProcessError::InputFull)
    ));
    assert!(matches!(
        supervisor.try_send_input(execution, &[0; CHUNK_BYTES + 1]),
        Err(ProcessError::InputTooLarge)
    ));
    ready(&mut supervisor, execution).await;
    sleep(Duration::from_millis(100)).await;
    // No output consumption: both OS streams and the shared output queue fill.
    assert!(
        timeout(Duration::from_millis(100), supervisor.settle(execution))
            .await
            .is_err()
    );
    assert_eq!(supervisor.in_flight(), 1);
    supervisor.cancel(task).unwrap();
    let report = settle(&mut supervisor, execution).await;
    assert!(report.cleanup_complete);
    assert!(!report.pipe_failed);
    assert_eq!(
        supervisor.snapshot(task).unwrap().state,
        TaskState::Cancelled
    );
    assert_eq!(supervisor.in_flight(), 0);
}

#[tokio::test]
async fn draining_both_streams_preserves_all_bytes_under_backpressure() {
    let mut supervisor = ProcessSupervisor::new(1, 1).unwrap();
    supervisor.submit().unwrap();
    let execution = supervisor
        .dispatch(spec("flood"), limits())
        .unwrap()
        .unwrap();
    let mut stdout = 0;
    let mut stderr = 0;
    timeout(Duration::from_secs(10), async {
        while let Some(chunk) = supervisor.recv_output(execution).await.unwrap() {
            assert!(chunk.bytes.len() <= CHUNK_BYTES);
            match chunk.stream {
                Stream::Stdout => stdout += chunk.bytes.len(),
                Stream::Stderr => stderr += chunk.bytes.len(),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(stdout, 4096 * 1024 + 6);
    assert_eq!(stderr, 4096 * 1024);
    assert_eq!(
        settle(&mut supervisor, execution).await.termination,
        Termination::Exited { success: true }
    );
}

#[tokio::test]
async fn run_deadline_reclaims_slot_without_claiming_success() {
    let mut supervisor = ProcessSupervisor::new(1, 1).unwrap();
    let task = supervisor.submit().unwrap();
    let mut short = limits();
    short.run_timeout = Duration::from_millis(500);
    let execution = supervisor.dispatch(spec("hang"), short).unwrap().unwrap();
    ready(&mut supervisor, execution).await;
    let report = settle(&mut supervisor, execution).await;
    assert_eq!(report.termination, Termination::TimedOut);
    assert_eq!(supervisor.snapshot(task).unwrap().state, TaskState::Failed);
    assert!(!supervisor.snapshot(task).unwrap().result_published);
    assert_eq!(supervisor.in_flight(), 0);
}

#[tokio::test]
async fn stdin_eof_and_old_execution_rejection_across_steps() {
    let mut supervisor = ProcessSupervisor::new(1, 1).unwrap();
    let task = supervisor.submit().unwrap();
    let worker = supervisor
        .dispatch(spec("echo"), limits())
        .unwrap()
        .unwrap();
    assert_eq!(ready(&mut supervisor, worker).await, "ready\n");
    supervisor
        .try_send_input(worker, "合成数据\n".as_bytes())
        .unwrap();
    supervisor.close_input(worker).unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = supervisor.recv_output(worker).await.unwrap() {
        if chunk.stream == Stream::Stdout {
            bytes.extend(chunk.bytes);
        }
    }
    assert_eq!(bytes, "合成数据\n".as_bytes());
    settle(&mut supervisor, worker).await;
    let provider = supervisor
        .dispatch(spec("success"), limits())
        .unwrap()
        .unwrap();
    assert_ne!(worker, provider);
    assert!(matches!(
        supervisor.settle(worker).await,
        Err(ProcessError::Scheduler(SchedulerError::StaleExecution))
    ));
    assert!(matches!(
        supervisor.close_input(worker),
        Err(ProcessError::Scheduler(SchedulerError::StaleExecution))
    ));
    assert_eq!(supervisor.in_flight(), 1);
    settle(&mut supervisor, provider).await;
    assert_eq!(supervisor.cancel(task).unwrap(), TaskState::Succeeded);
    assert!(supervisor.take_finished(task).unwrap().result_published);
    assert!(matches!(
        supervisor.settle(provider).await,
        Err(ProcessError::Scheduler(SchedulerError::StaleExecution))
    ));
}

#[tokio::test]
async fn spawn_failure_nonzero_exit_and_invalid_limits() {
    let mut supervisor = ProcessSupervisor::new(2, 1).unwrap();
    let first = supervisor.submit().unwrap();
    let second = supervisor.submit().unwrap();
    let mut invalid = limits();
    invalid.pipe_chunks = 0;
    assert!(matches!(
        supervisor.dispatch(spec("success"), invalid),
        Err(ProcessError::InvalidLimits)
    ));
    assert_eq!(supervisor.snapshot(first).unwrap().state, TaskState::Queued);
    let mut missing = spec("success");
    missing
        .program
        .set_file_name("videosift-no-such-executable");
    assert!(matches!(
        supervisor.dispatch(missing, limits()),
        Err(ProcessError::Spawn { .. })
    ));
    assert_eq!(supervisor.snapshot(first).unwrap().state, TaskState::Failed);
    assert_eq!(supervisor.in_flight(), 0);
    let execution = supervisor
        .dispatch(spec("fail"), limits())
        .unwrap()
        .unwrap();
    assert_eq!(execution.task(), second);
    assert_eq!(
        settle(&mut supervisor, execution).await.termination,
        Termination::Exited { success: false }
    );
    assert_eq!(
        supervisor.snapshot(second).unwrap().state,
        TaskState::Failed
    );
}

#[tokio::test]
async fn dropping_supervisor_requests_tree_cleanup() {
    let mut supervisor = ProcessSupervisor::new(1, 1).unwrap();
    supervisor.submit().unwrap();
    let execution = supervisor
        .dispatch(spec("tree"), limits())
        .unwrap()
        .unwrap();
    let message = ready(&mut supervisor, execution).await;
    let pid = message
        .trim()
        .strip_prefix("descendant=")
        .unwrap()
        .parse()
        .unwrap();
    drop(supervisor);
    assert_dead(pid).await;
}

#[tokio::test]
async fn independent_processes_keep_separate_slots_and_cancellation() {
    let mut supervisor = ProcessSupervisor::new(2, 2).unwrap();
    let first = supervisor.submit().unwrap();
    let second = supervisor.submit().unwrap();
    let a = supervisor
        .dispatch(spec("cooperate"), limits())
        .unwrap()
        .unwrap();
    let b = supervisor
        .dispatch(spec("cooperate"), limits())
        .unwrap()
        .unwrap();
    ready(&mut supervisor, a).await;
    ready(&mut supervisor, b).await;
    assert_eq!(supervisor.in_flight(), 2);
    supervisor.cancel(first).unwrap();
    settle(&mut supervisor, a).await;
    assert_eq!(supervisor.in_flight(), 1);
    assert_eq!(
        supervisor.snapshot(second).unwrap().state,
        TaskState::Running
    );
    supervisor.close_input(b).unwrap();
    settle(&mut supervisor, b).await;
    assert!(supervisor.snapshot(second).unwrap().worker_completed);
    assert_eq!(supervisor.in_flight(), 0);
}

#[tokio::test]
async fn cancellation_before_settlement_rejects_already_exited_result() {
    let mut supervisor = ProcessSupervisor::new(1, 1).unwrap();
    let task = supervisor.submit().unwrap();
    let execution = supervisor
        .dispatch(spec("success"), limits())
        .unwrap()
        .unwrap();
    // EOF proves both output pumps have finished; settlement remains explicit.
    assert!(supervisor.recv_output(execution).await.unwrap().is_none());
    supervisor.cancel(task).unwrap();
    settle(&mut supervisor, execution).await;
    let snapshot = supervisor.take_finished(task).unwrap();
    assert_eq!(snapshot.state, TaskState::Cancelled);
    assert!(!snapshot.worker_completed && !snapshot.result_published);
    assert!(
        supervisor
            .dispatch(spec("success"), limits())
            .unwrap()
            .is_none()
    );
}

#[cfg(windows)]
#[tokio::test]
async fn windows_job_closes_when_host_exits_without_destructors() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut host = tokio::process::Command::new(env!("CARGO_BIN_EXE_process-test-helper"))
        .arg("host-exit")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pipe = host.stdout.take().unwrap();
    let mut line = String::new();
    timeout(
        Duration::from_secs(10),
        BufReader::new(pipe).read_line(&mut line),
    )
    .await
    .unwrap()
    .unwrap();
    let pid = line
        .trim()
        .strip_prefix("descendant=")
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        timeout(Duration::from_secs(5), host.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert_dead(pid).await;
}

async fn assert_dead(pid: u32) {
    timeout(Duration::from_secs(5), async {
        while process_alive(pid) {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("descendant still running after cleanup");
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: the handle is used only for a zero-time wait and closed once.
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(handle) => {
                let active = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
                CloseHandle(handle).unwrap();
                active
            }
            Err(error) => {
                assert_eq!(
                    error.code(),
                    windows::core::HRESULT::from_win32(87),
                    "unexpected process query failure"
                );
                false
            }
        }
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // Linux CI: a zombie is no longer running; its adopting parent owns reaping.
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, tail)| !tail.starts_with('Z'))
    })
}
