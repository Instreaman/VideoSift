//! Observe an exit without releasing the group leader's process ID.

use std::io;

use nix::{errno::Errno, libc, unistd::Pid};

pub(super) fn exit_status(pid: Pid) -> io::Result<Option<bool>> {
    // POSIX reports no status through a zero si_pid with WNOHANG. Initialize the
    // whole buffer so that this also works when an implementation leaves it alone.
    // SAFETY: siginfo_t is a C data structure that can be initialized to zero.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: info is writable, P_PID limits the operation to the supplied
        // child, and WNOWAIT leaves its exit status available for later reaping.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid.as_raw() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        match Errno::result(result) {
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
            Ok(_) => break,
        }
    }
    // SAFETY: successful waitid initialized the SIGCHLD fields when a child exited.
    if unsafe { info.si_pid() } == 0 {
        return Ok(None);
    }
    // SAFETY: the nonzero PID indicates a waitid result for this child.
    Ok(Some(
        info.si_code == libc::CLD_EXITED && unsafe { info.si_status() } == 0,
    ))
}
