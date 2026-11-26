//! Own the recipe process tree until its output is collected and the tree is empty.

use std::process::{ChildStderr, ChildStdout};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
pub(crate) use platform::run_group_anchor;
pub(crate) use platform::{Process, spawn};

pub(crate) struct Spawned {
    pub(crate) process: Process,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
mod platform {
    use std::ffi::OsStr;
    use std::io::{self, Read};
    use std::os::unix::process::CommandExt as _;
    use std::process::{Child, Command, ExitStatus, Stdio};

    use rustix::io::{Errno, retry_on_intr};
    use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, getpgid, getppid, kill_process_group, setpgid, test_kill_process_group, waitid};

    use super::Spawned;

    const ANCHOR_ARG: &str = "--internal-recipe-group-anchor";
    #[cfg(test)]
    const ANCHOR_FIXTURE: &str = "ARTCRAFT_TEST_RECIPE_GROUP_ANCHOR";

    pub(crate) struct Process {
        child: Child,
        anchor: Child,
    }

    pub(crate) fn run_group_anchor() -> Option<io::Result<()>> {
        (std::env::args_os().nth(1).as_deref() == Some(OsStr::new(ANCHOR_ARG))).then(leave_group)
    }

    fn leave_group() -> io::Result<()> {
        let parent = getppid().ok_or_else(|| io::Error::other("Recipe group anchor has no parent"))?;
        let worker_group = getpgid(Some(parent))?;
        if io::stdin().read(&mut [0])? != 0 {
            return Err(io::Error::other("Unexpected input to recipe group anchor"));
        }
        // Only leave after the parent has placed the recipe in our group. Moving the anchor
        // out lets a kernel group probe distinguish descendants from the retained anchor PID.
        setpgid(None, Some(worker_group)).map_err(io::Error::from)
    }

    #[cfg(test)]
    #[test]
    fn anchor_fixture() -> io::Result<()> {
        if std::env::var_os(ANCHOR_FIXTURE).is_some() { leave_group() } else { Ok(()) }
    }

    fn spawn_anchor() -> io::Result<Child> {
        let mut command = Command::new(std::env::current_exe()?);
        #[cfg(not(test))]
        command.arg(ANCHOR_ARG);
        #[cfg(test)]
        command.args(["--exact", "recipe_process::platform::anchor_fixture", "--nocapture"]).env(ANCHOR_FIXTURE, "1");
        command.process_group(0).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
    }

    pub(crate) fn spawn(mut command: Command) -> io::Result<Spawned> {
        // The Linux worker owns one recipe per invocation. Adopt its orphans so that a
        // zombie left by a container's init cannot keep a finished group busy indefinitely.
        #[cfg(target_os = "linux")]
        rustix::process::set_child_subreaper(Some(rustix::process::getpid()))?;
        let mut anchor = spawn_anchor()?;
        let spawned = command.process_group(Pid::from_child(&anchor).as_raw_nonzero().get()).spawn();
        drop(anchor.stdin.take());
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                let _ = anchor.kill();
                let _ = anchor.wait();
                return Err(error);
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Ok(Spawned { process: Process { child, anchor }, stdout, stderr })
    }

    fn anchor_success(anchor: &Child) -> io::Result<Option<bool>> {
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        let status = retry_on_intr(|| waitid(WaitId::Pid(Pid::from_child(anchor)), options))?;
        Ok(status.map(|status| status.exit_status() == Some(0)))
    }

    #[cfg(target_os = "linux")]
    fn reap_group_children(group: Pid) -> io::Result<()> {
        use rustix::process::{WaitOptions, waitpgid};

        // A bounded batch prevents a very large group from delaying the deadline check.
        for _ in 0..128 {
            match retry_on_intr(|| waitpgid(group, WaitOptions::NOHANG)) {
                Ok(Some(_)) => {}
                Ok(None) | Err(Errno::CHILD) => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    impl Process {
        pub(crate) fn leader_success(&mut self) -> io::Result<Option<bool>> {
            Ok(self.child.try_wait()?.map(|status| status.success()))
        }

        pub(crate) fn is_empty(&self) -> io::Result<bool> {
            match anchor_success(&self.anchor)? {
                Some(true) => {}
                Some(false) => return Err(io::Error::other("The recipe group anchor could not leave its group")),
                None => return Ok(false),
            }
            let group = Pid::from_child(&self.anchor);
            #[cfg(target_os = "linux")]
            reap_group_children(group)?;
            match test_kill_process_group(group) {
                Err(Errno::SRCH) => Ok(true),
                Ok(()) | Err(Errno::PERM) => Ok(false),
                Err(error) => Err(error.into()),
            }
        }

        pub(crate) fn stop(mut self) -> io::Result<ExitStatus> {
            // The anchor has never been reaped, so its PID still reserves the numeric PGID.
            // Refuse to signal if the kernel no longer recognizes it as our child.
            let _ = anchor_success(&self.anchor)?;
            let signalled = kill_process_group(Pid::from_child(&self.anchor), Signal::KILL);
            // Stop the directly owned recipe even if it left the group before cancellation.
            if self.child.try_wait()?.is_none()
                && let Err(error) = self.child.kill()
                && self.child.try_wait()?.is_none()
            {
                let _ = self.anchor.wait();
                return Err(error);
            }
            let status = self.reap()?;
            match signalled {
                Ok(()) | Err(Errno::SRCH) => Ok(status),
                Err(error) => Err(error.into()),
            }
        }

        pub(crate) fn reap(mut self) -> io::Result<ExitStatus> {
            let status = self.child.wait();
            self.anchor.wait()?;
            status
        }
    }
}

#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
mod platform {
    use std::io;
    use std::process::{Child, Command, ExitStatus};

    use subc_jobobject::{ContainedChild, spawn_contained};

    use super::Spawned;

    pub(crate) struct Process(ContainedChild<Child>);

    pub(crate) fn spawn(mut command: Command) -> io::Result<Spawned> {
        // The wrapper assigns the process to the unnamed job before resuming it.
        let mut child = spawn_contained(&mut command)?;
        let stdout = child.child.stdout.take();
        let stderr = child.child.stderr.take();
        Ok(Spawned { process: Process(child), stdout, stderr })
    }

    impl Process {
        pub(crate) fn leader_success(&mut self) -> io::Result<Option<bool>> {
            Ok(self.0.child.try_wait()?.map(|status| status.success()))
        }

        pub(crate) fn is_empty(&self) -> io::Result<bool> {
            self.0.job.process_count().map(|count| count == 0)
        }

        pub(crate) fn stop(mut self) -> io::Result<ExitStatus> {
            // The job handle remains valid after the leader exits. Never look up a numeric PID.
            let signalled = self.0.job.terminate();
            if signalled.is_err() && self.0.child.try_wait()?.is_none() {
                self.0.child.kill()?;
            }
            let status = self.reap()?;
            signalled.map(|()| status)
        }

        pub(crate) fn reap(mut self) -> io::Result<ExitStatus> {
            self.0.child.wait()
        }
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos", target_os = "freebsd")))]
mod platform {
    use std::io;
    use std::process::{Command, ExitStatus};

    use super::Spawned;

    pub(crate) struct Process;

    fn unsupported() -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, "Recipe tree ownership is unavailable on this platform")
    }

    pub(crate) fn spawn(_command: Command) -> io::Result<Spawned> {
        Err(unsupported())
    }

    impl Process {
        pub(crate) fn leader_success(&mut self) -> io::Result<Option<bool>> {
            Err(unsupported())
        }

        pub(crate) fn is_empty(&self) -> io::Result<bool> {
            Err(unsupported())
        }

        pub(crate) fn stop(self) -> io::Result<ExitStatus> {
            Err(unsupported())
        }

        pub(crate) fn reap(self) -> io::Result<ExitStatus> {
            Err(unsupported())
        }
    }
}
