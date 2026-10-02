//! Short-lived helper processes the daemon runs in the background (a CLI asked for its
//! models): no console window on Windows, and on unix a process group of their own, so
//! one that is given up on is ended together with whatever it started.
use std::process::{Child, Command};

/// Sets up `cmd` to run quietly: on Windows without a console window (the daemon has
/// none, so each child would open its own); on unix in its own process group.
pub fn quiet_child(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
}

/// Ends a child started with `quiet_child` and reaps it: on unix its whole process group
/// (a node shim and the native CLI it started), on Windows the child itself.
pub fn kill_quiet_child(child: &mut Child) {
    #[cfg(unix)]
    {
        // SAFETY: kill(2) takes no pointers. The group id is our child's pid, and a pid
        // is not reused while a process group of that id still has members, nor while
        // the child is not yet reaped.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
    }
    #[cfg(windows)]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_child_leads_its_own_process_group_and_is_killed_with_it() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        quiet_child(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        // SAFETY: getpgid(2) takes no pointers.
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        kill_quiet_child(&mut child);
        assert!(child.try_wait().unwrap().is_some(), "ended and reaped");
    }
}
