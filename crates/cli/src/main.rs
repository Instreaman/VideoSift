use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use videosift_core::mock_run::{MockRun, RunError};
use videosift_core::storage::{StorageError, TaskStore};
use videosift_core::task::TaskState;

#[derive(Parser)]
#[command(
    name = "videosift",
    version,
    about = "Run the VideoSift synthetic Mock pipeline"
)]
struct Cli {
    /// Existing private directory for this CLI's tasks and cache.
    #[arg(long)]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create and execute a task with the built-in synthetic input.
    Run {
        /// Wait at the Worker checkpoint for 'continue' or 'cancel' on stdin.
        #[arg(long)]
        interactive: bool,
    },
    /// Show the durable state, checkpoint and published output reference.
    Status { task_id: TaskId },
    /// Cancel a task in an idle store. For live work use the foreground input.
    Cancel { task_id: TaskId },
    /// Explicitly requeue and execute an interrupted task.
    Resume {
        task_id: TaskId,
        #[arg(long)]
        interactive: bool,
    },
}

type TaskId = std::num::NonZeroI64;

struct Failure {
    code: &'static str,
    message: &'static str,
    recoverable: bool,
    exit: u8,
}

impl Failure {
    fn validation(message: &'static str) -> Self {
        Self {
            code: "validation",
            message,
            recoverable: true,
            exit: 2,
        }
    }
}

impl From<StorageError> for Failure {
    fn from(err: StorageError) -> Self {
        let message = match err {
            StorageError::UnknownTask(_) => {
                return Self::validation("Unknown task ID in this store");
            }
            StorageError::Transition(_) => {
                return Self::validation("Command is invalid in this task state");
            }
            StorageError::AlreadyOpen => {
                "Store is busy; use the foreground process for live control"
            }
            StorageError::NewerVersion(_) => "Task database requires a newer version of VideoSift",
            StorageError::ForeignDatabase => "Unrecognized task database",
            StorageError::CacheIntegrity => "Cache artifact failed integrity verification",
            _ => "Could not access the task store or cache; check the private directory and disk",
        };
        Self {
            code: "storage",
            message,
            recoverable: true,
            exit: 1,
        }
    }
}

impl From<RunError> for Failure {
    fn from(err: RunError) -> Self {
        match err {
            RunError::Storage(err) => err.into(),
            RunError::Scheduler(_) => Self {
                code: "internal",
                message: "Mock scheduler could not complete the step",
                recoverable: false,
                exit: 1,
            },
        }
    }
}

impl From<io::Error> for Failure {
    fn from(_: io::Error) -> Self {
        Self {
            code: "internal",
            message: "CLI input or output stream failed",
            recoverable: true,
            exit: 1,
        }
    }
}

fn emit(value: Value) -> Result<(), Failure> {
    let mut out = io::stdout().lock();
    writeln!(out, "{value}")?;
    out.flush()?;
    Ok(())
}

fn status(store: &TaskStore, id: i64) -> Result<(), Failure> {
    let task = store.task(id)?;
    emit(
        json!({"event": "task", "task_id": id.to_string(), "state": task.state,
        "checkpoint": task.checkpoint, "output_ref": task.output_ref}),
    )
}

fn continue_run(input: impl BufRead) -> Result<bool, Failure> {
    let mut bytes = Vec::new();
    input.take(16).read_until(b'\n', &mut bytes)?;
    let line = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    match line.strip_suffix(b"\r").unwrap_or(line) {
        b"continue" => Ok(true),
        b"cancel" | b"" => Ok(false),
        _ => Err(Failure::validation("Expected continue or cancel on stdin")),
    }
}

fn execute(cli: Cli, id: &mut Option<i64>) -> Result<u8, Failure> {
    *id = match &cli.command {
        Command::Run { .. } => None,
        Command::Status { task_id }
        | Command::Cancel { task_id }
        | Command::Resume { task_id, .. } => Some(task_id.get()),
    };
    if id.is_some_and(|n| n < 1) {
        return Err(Failure::validation("Expected a positive task ID"));
    }
    let mut store = TaskStore::open(&cli.data_dir)?;
    if store.startup_cache_cleanup_error().is_some() {
        eprintln!(
            r#"{{"event":"warning","code":"storage","message":"Orphan cache cleanup needs retry"}}"#
        );
    }
    let (task, interactive) = match cli.command {
        Command::Run { interactive } => (store.submit()?, interactive),
        Command::Resume {
            task_id,
            interactive,
        } => {
            store.resume(task_id.get())?;
            (task_id.get(), interactive)
        }
        Command::Status { task_id } => {
            status(&store, task_id.get())?;
            return Ok(0);
        }
        Command::Cancel { task_id } => {
            store.cancel(task_id.get())?;
            status(&store, task_id.get())?;
            return Ok(0);
        }
    };
    *id = Some(task);
    status(&store, task)?;
    let run = MockRun::start(&mut store, task)?;
    emit(
        json!({"event": "worker_checkpoint", "task_id": task.to_string(), "state": TaskState::Running,
        "cache_hit": run.worker_cache_hit}),
    )?;
    if interactive {
        match continue_run(io::stdin().lock()) {
            Ok(true) => {}
            decision => {
                run.cancel()?;
                status(&store, task)?;
                decision?;
                return Ok(3);
            }
        }
    }
    let hit = run.complete()?;
    emit(json!({"event": "provider_completed", "task_id": task.to_string(), "cache_hit": hit}))?;
    status(&store, task)?;
    Ok(0)
}

fn main() -> ExitCode {
    let mut task_id = None;
    match execute(Cli::parse(), &mut task_id) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!(
                "{}",
                json!({"protocol_version": "0.1.0", "code": err.code,
                "message": err.message, "task_id": task_id.map(|id| id.to_string()), "recoverable": err.recoverable})
            );
            ExitCode::from(err.exit)
        }
    }
}
