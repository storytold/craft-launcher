//! Own the recipe process tree until its output has been collected.

use std::process::{ChildStderr, ChildStdout};

pub(crate) use platform::{Process, spawn};

pub(crate) struct Spawned {
    pub(crate) process: Process,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
mod platform {
    use std::io;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Child, Command, ExitStatus};

    use rustix::io::{Errno, retry_on_intr};
    use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

    use super::Spawned;

    pub(crate) struct Process(Child);

    pub(crate) fn spawn(mut command: Command) -> io::Result<Spawned> {
        let mut child = command.process_group(0).spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Ok(Spawned { process: Process(child), stdout, stderr })
    }

    impl Process {
        pub(crate) fn leader_success(&mut self) -> io::Result<Option<bool>> {
            // Keep the exited leader waitable so its PID still reserves our process group ID.
            let id = Pid::from_child(&self.0);
            let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
            let status = retry_on_intr(|| waitid(WaitId::Pid(id), options))?;
            Ok(status.map(|status| status.exit_status() == Some(0)))
        }

        pub(crate) fn stop(mut self) -> io::Result<ExitStatus> {
            // Refuse to signal if the leader is no longer our waitable child.
            let _ = self.leader_success()?;
            let signalled = kill_process_group(Pid::from_child(&self.0), Signal::KILL);
            if signalled.is_err() && self.leader_success()?.is_none() {
                self.0.kill()?;
            }
            let status = self.reap()?;
            match signalled {
                Ok(()) | Err(Errno::SRCH) => Ok(status),
                Err(error) => Err(error.into()),
            }
        }

        pub(crate) fn reap(mut self) -> io::Result<ExitStatus> {
            self.0.wait()
        }
    }
}

#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
mod platform {
    use std::io;
    use std::process::{Command, ExitStatus};

    use process_wrap::std::{ChildWrapper, CommandWrap, CreationFlags, JobObject};

    use super::{CREATE_NO_WINDOW, Spawned};

    pub(crate) struct Process(Box<dyn ChildWrapper>);

    pub(crate) fn spawn(command: Command) -> io::Result<Spawned> {
        let mut command = CommandWrap::from(command);
        let mut flags = CreationFlags(Default::default());
        flags.0.0 = CREATE_NO_WINDOW;
        // Assignment to the unnamed job happens while the recipe is still suspended.
        let mut child = command.wrap(flags).wrap(JobObject).spawn()?;
        let stdout = child.stdout().take();
        let stderr = child.stderr().take();
        Ok(Spawned { process: Process(child), stdout, stderr })
    }

    impl Process {
        pub(crate) fn leader_success(&mut self) -> io::Result<Option<bool>> {
            Ok(self.0.inner_mut().try_wait()?.map(|status| status.success()))
        }

        pub(crate) fn stop(mut self) -> io::Result<ExitStatus> {
            // The job handle remains valid after the leader exits. Never look up a numeric PID.
            let signalled = self.0.start_kill();
            if signalled.is_err() && self.leader_success()?.is_none() {
                self.0.inner_mut().start_kill()?;
            }
            let status = self.reap()?;
            signalled.map(|()| status)
        }

        pub(crate) fn reap(mut self) -> io::Result<ExitStatus> {
            // Only reap the leader here. Log collection has its own bounded wait for the pipes.
            self.0.inner_mut().wait()
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

        pub(crate) fn stop(self) -> io::Result<ExitStatus> {
            Err(unsupported())
        }

        pub(crate) fn reap(self) -> io::Result<ExitStatus> {
            Err(unsupported())
        }
    }
}
