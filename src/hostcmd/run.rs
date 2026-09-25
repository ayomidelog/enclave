//! Running a host command under its deadline.
//!
//! Every spawn Enclave makes for host work goes through here, so the deadline,
//! the output cap, and the structured failure are in one place rather than
//! repeated by each caller.

use super::*;

impl HostCommand {
    /// Run the command, stopping it if it outlives its deadline.
    ///
    /// The child is placed in its own process group so a timeout stops the whole
    /// tree rather than only the process that was started.
    pub(crate) fn run(self) -> Result<HostOutput> {
        // Every host command is a fork and an exec, and on a loaded host that is
        // where a lifecycle operation's time goes. Timing the whole call is what
        // makes that attributable, and it is gated on the perf switch so a normal
        // daemon pays nothing for it.
        let timing = crate::perf::enabled().then(|| crate::perf::Timer::new("hostcmd.run"));
        let result = self.run_timed();
        drop(timing);
        result
    }

    fn run_timed(mut self) -> Result<HostOutput> {
        let program = self.program.to_string_lossy().into_owned();
        let args = self
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        if self.capture_output {
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
        } else {
            command.stdout(Stdio::null());
            command.stderr(Stdio::null());
        }
        // A timeout has to reach the whole tree. A command that forks would
        // otherwise leave descendants holding the pipes and the workspace.
        let netns_pid = self.netns_pid;
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(move || {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(pid) = netns_pid {
                    enter_network_namespace(pid)?;
                }
                Ok(())
            });
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                crate::perf::record_host_command(false, true);
                return Err(HostCommandError {
                    program,
                    args,
                    kind: HostCommandFailure::Spawn,
                    status: None,
                    stderr: error.to_string(),
                    timeout: self.timeout,
                }
                .into());
            }
        };
        let child_pid = child.id();

        // Feed stdin from its own thread: a payload larger than the pipe buffer
        // would otherwise deadlock against a command that writes before it reads.
        let stdin_writer = self.stdin.take().map(|bytes| {
            let mut handle = child.stdin.take().expect("stdin is piped when set");
            std::thread::spawn(move || {
                let _ = handle.write_all(&bytes);
                let _ = handle.flush();
            })
        });

        let readers = if self.capture_output {
            let stdout = child.stdout.take().expect("stdout is piped");
            let stderr = child.stderr.take().expect("stderr is piped");
            let (stdout_sender, stdout_receiver) = std::sync::mpsc::channel();
            let (stderr_sender, stderr_receiver) = std::sync::mpsc::channel();
            let cap = self.output_cap;
            std::thread::spawn(move || {
                let _ = stdout_sender.send(capture::read_capped(stdout, cap));
            });
            std::thread::spawn(move || {
                let _ = stderr_sender.send(capture::read_capped(stderr, cap));
            });
            Some((stdout_receiver, stderr_receiver))
        } else {
            None
        };

        let finished = wait_until_deadline(&mut child, self.timeout)
            .with_context(|| format!("failed to wait for host command {}", self.describe()))?;

        if !finished {
            // Stop the group, then the process itself in case the group change
            // did not take effect before the deadline.
            unsafe { libc::killpg(child_pid as i32, libc::SIGKILL) };
            let _ = child.kill();
        }
        let status = child
            .wait()
            .with_context(|| format!("failed to reap host command {}", self.describe()))?;
        if let Some(writer) = stdin_writer {
            let _ = writer.join();
        }
        let (stdout, stderr) = match readers {
            Some((stdout_receiver, stderr_receiver)) => (
                stdout_receiver
                    .recv_timeout(OUTPUT_GRACE)
                    .unwrap_or_default(),
                stderr_receiver
                    .recv_timeout(OUTPUT_GRACE)
                    .unwrap_or_default(),
            ),
            None => (Vec::new(), Vec::new()),
        };

        let failed = !status.success() || !finished;
        crate::perf::record_host_command(!finished, failed);

        if !finished {
            return Err(HostCommandError {
                program,
                args,
                kind: HostCommandFailure::TimedOut,
                status: status.code(),
                stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
                timeout: self.timeout,
            }
            .into());
        }

        Ok(HostOutput {
            status,
            stdout,
            stderr,
        })
    }

    /// Run the command and fail unless it exited successfully.
    pub(crate) fn run_checked(self) -> Result<HostOutput> {
        let program = self.program.to_string_lossy().into_owned();
        let args = self
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let timeout = self.timeout;
        let output = self.run()?;
        if output.status.success() {
            return Ok(output);
        }
        Err(HostCommandError {
            program,
            args,
            kind: HostCommandFailure::ExitStatus,
            status: output.status.code(),
            stderr: output.stderr_text(),
            timeout,
        }
        .into())
    }
}
