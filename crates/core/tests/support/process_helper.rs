//! Synthetic subprocess used by integration tests; not an application CLI.
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

fn main() -> io::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        #[cfg(windows)]
        "host-exit" => {
            use videosift_core::process::{Limits, ProcessSpec, ProcessSupervisor};
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let mut supervisor = ProcessSupervisor::new(1, 1).unwrap();
                    supervisor.submit().unwrap();
                    let execution = supervisor
                        .dispatch(
                            ProcessSpec {
                                program: std::env::current_exe().unwrap(),
                                args: vec!["tree".into()],
                            },
                            Limits {
                                pipe_chunks: 2,
                                run_timeout: Duration::from_secs(10),
                                cancel_grace: Duration::from_millis(300),
                                cleanup_timeout: Duration::from_secs(5),
                            },
                        )
                        .unwrap()
                        .unwrap();
                    let mut bytes = Vec::new();
                    while !bytes.contains(&b'\n') {
                        bytes.extend(
                            supervisor
                                .recv_output(execution)
                                .await
                                .unwrap()
                                .unwrap()
                                .bytes,
                        );
                    }
                    io::stdout().write_all(&bytes).unwrap();
                    io::stdout().flush().unwrap();
                    // Deliberately bypass Rust destructors to verify OS job-handle cleanup.
                    std::process::exit(0);
                });
        }
        "echo" => {
            println!("ready");
            let mut buffer = [0; 4096];
            loop {
                let count = io::stdin().read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                io::stdout().write_all(&buffer[..count])?;
            }
            eprintln!("done");
        }
        "cooperate" => {
            println!("ready");
            let mut buffer = [0; 4096];
            while io::stdin().read(&mut buffer)? != 0 {}
        }
        "hang" => {
            println!("ready");
            thread::sleep(Duration::from_secs(60));
        }
        "tree" | "orphan" => {
            let child = Command::new(std::env::current_exe()?)
                .arg("hang")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            println!("descendant={}", child.id());
            // Intentionally leave this synthetic descendant for the supervisor.
            // An outer test timeout and the process-tree guard bound its lifetime.
            if mode == "tree" {
                thread::sleep(Duration::from_secs(60));
            }
        }
        "flood" => {
            println!("ready");
            let writer = thread::spawn(|| -> io::Result<()> {
                let bytes = [b'e'; 4096];
                for _ in 0..1024 {
                    io::stderr().write_all(&bytes)?;
                }
                Ok(())
            });
            let bytes = [b'o'; 4096];
            for _ in 0..1024 {
                io::stdout().write_all(&bytes)?;
            }
            writer
                .join()
                .map_err(|_| io::Error::other("writer panic"))??;
        }
        "fail" => std::process::exit(7),
        "success" => {}
        _ => return Err(io::Error::other("unknown test mode")),
    }
    Ok(())
}
