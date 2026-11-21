//! Process-tree ownership. No shell expansion or unbounded output collection.

use std::io;
use std::process::Stdio;
use std::time::Duration;

use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::process::{Child, Command};

use super::ProcessSpec;

pub(super) struct ProcessTree {
    child: Box<dyn ChildWrapper>,
    armed: bool,
    #[cfg(unix)]
    pgid: nix::unistd::Pid,
    #[cfg(windows)]
    job: super::job::TrackedJob,
}

impl ProcessTree {
    pub fn spawn(spec: &ProcessSpec) -> io::Result<Self> {
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut command = CommandWrap::from(command);
        command.wrap(KillOnDrop);
        #[cfg(windows)]
        let job = super::job::TrackedJob::new()?;
        #[cfg(windows)]
        {
            // JobObject suspends creation until assignment. Preserve a hidden window.
            command.wrap(process_wrap::tokio::CreationFlags(
                windows::Win32::System::Threading::CREATE_NO_WINDOW,
            ));
            command.wrap(process_wrap::tokio::JobObject);
            command.wrap(job.clone());
        }
        #[cfg(unix)]
        command.wrap(process_wrap::tokio::ProcessGroup::leader());
        let child = command.spawn()?;
        #[cfg(unix)]
        let pgid = nix::unistd::Pid::from_raw(
            child
                .id()
                .ok_or_else(|| io::Error::other("missing process id"))? as i32,
        );
        Ok(Self {
            child,
            armed: true,
            #[cfg(unix)]
            pgid,
            #[cfg(windows)]
            job,
        })
    }

    pub fn parent(&mut self) -> io::Result<&mut Child> {
        // SAFETY: only JobObject/ProcessGroup/KillOnDrop wrap this child. Callers
        // take pipes or wait on the parent; they never replace it or bypass the
        // separate tree termination/wait, and the wrapper remains owned here.
        unsafe { self.child.try_inner_child_mut() }
            .ok_or_else(|| io::Error::other("missing native child"))
    }

    pub fn terminate(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        #[cfg(unix)]
        {
            // Check the leader is still ours before signaling its group. Nobody
            // else in the host may reap this child while ProcessTree owns it.
            super::unix::exit_status(self.pgid)?;
        }
        match self.child.start_kill() {
            // An already empty Unix process group needs no further signal.
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(nix::errno::Errno::ESRCH as i32) => Ok(()),
            result => result,
        }
    }

    pub async fn wait_parent(&mut self) -> io::Result<bool> {
        // On Unix the unreaped root reserves the PID used as our group ID.
        #[cfg(unix)]
        loop {
            if let Some(success) = super::unix::exit_status(self.pgid)? {
                return Ok(success);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        #[cfg(windows)]
        return Ok(self.parent()?.wait().await?.success());
    }

    /// Called only after the last group termination request has succeeded.
    pub async fn wait_tree(&mut self) -> io::Result<()> {
        // Disarm before polling native wait: once it reaps the leader, no code
        // (including Drop after timeout) may signal this numeric PGID again.
        #[cfg(unix)]
        {
            self.armed = false;
        }
        self.parent()?.wait().await?;

        // On Windows, query the job instead of relying on completion notifications.
        #[cfg(windows)]
        while !self.job.is_empty()? {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        #[cfg(unix)]
        loop {
            // Signal 0 only probes existence. After reaping the root a reused
            // number may delay confirmation, but can no longer target a group.
            match nix::sys::signal::killpg(self.pgid, None) {
                Err(nix::errno::Errno::ESRCH) => break,
                Err(error) => return Err(error.into()),
                Ok(()) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        self.armed = false;
        Ok(())
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        // Last-resort cleanup on runtime shutdown/panic; normal cleanup is awaited.
        let _ = self.terminate();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::process::unix::exit_status;
    use nix::errno::Errno;
    use tokio::time::timeout;

    fn successful_child() -> ProcessTree {
        ProcessTree::spawn(&ProcessSpec {
            program: "/bin/true".into(),
            args: vec![],
        })
        .unwrap()
    }

    #[tokio::test]
    async fn group_signal_precedes_reaping_the_leader() {
        let mut tree = successful_child();
        let pgid = tree.pgid;
        assert!(
            timeout(Duration::from_secs(5), tree.wait_parent())
                .await
                .unwrap()
                .unwrap()
        );
        // Observe exit twice: the leader must remain waitable and reserve its PID
        // when the runner requests group termination after a normal/cooperative exit.
        assert_eq!(exit_status(pgid).unwrap(), Some(true));
        tree.terminate().unwrap();
        timeout(Duration::from_secs(5), tree.wait_tree())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            exit_status(pgid).unwrap_err().raw_os_error(),
            Some(Errno::ECHILD as i32)
        );
        assert!(!tree.armed);
        tree.terminate().unwrap();
    }

    #[tokio::test]
    async fn group_signal_is_rejected_if_the_leader_was_already_reaped() {
        let mut tree = successful_child();
        // Simulate loss of ownership without trying to recycle any OS PID.
        timeout(Duration::from_secs(5), tree.parent().unwrap().wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            tree.terminate().unwrap_err().raw_os_error(),
            Some(Errno::ECHILD as i32)
        );
    }
}
