//! Bounded execution of host commands.
//!
//! Enclave drives its host work through external programs: `ip`, `mount`,
//! `umount`, `iptables`, `losetup`, `mkfs.ext4`, `resize2fs`, and `e2fsck`. A
//! command that never returns would otherwise hold a daemon worker, and with it
//! a registry lock or a lifecycle lease, for as long as the process lives.
//!
//! Every call through this module therefore has three properties:
//!
//! - a deadline, so no command can outlive its budget;
//! - capped output, so a chatty command cannot exhaust daemon memory;
//! - a structured failure, so a caller can tell a timeout from a non-zero exit
//!   without matching on message text.
//!
//! Commands that are expected to be long lived (`debootstrap`, the session
//! helper, the workspace runtime) do not belong here; they have their own
//! supervision.

mod capture;

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};

use capture::{wait_until_deadline, OUTPUT_GRACE};

/// Default deadline for a host command.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Hard upper bound on a configured deadline. A command allowed to run longer
/// than this should be supervised rather than awaited.
pub(crate) const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

/// Default cap on each of stdout and stderr.
pub(crate) const DEFAULT_OUTPUT_CAP: usize = 1024 * 1024;

/// How a host command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostCommandFailure {
    /// The program could not be started at all.
    Spawn,
    /// The deadline expired and the command was stopped.
    TimedOut,
    /// The command ran to completion with a non-zero status.
    ExitStatus,
}

/// A failed host command, carrying the detail a caller needs to report it.
#[derive(Debug)]
pub(crate) struct HostCommandError {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) kind: HostCommandFailure,
    pub(crate) status: Option<i32>,
    pub(crate) stderr: String,
    pub(crate) timeout: Duration,
}

impl std::fmt::Display for HostCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let command = self.describe();
        match self.kind {
            HostCommandFailure::Spawn => write!(formatter, "failed to run: {command}"),
            HostCommandFailure::TimedOut => write!(
                formatter,
                "{command} did not finish within {:?} and was stopped",
                self.timeout
            ),
            HostCommandFailure::ExitStatus => {
                let status = self
                    .status
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_string());
                if self.stderr.is_empty() {
                    write!(formatter, "{command} failed (status {status})")
                } else {
                    write!(
                        formatter,
                        "{command} failed (status {status}): {}",
                        self.stderr
                    )
                }
            }
        }
    }
}

impl std::error::Error for HostCommandError {}

impl HostCommandError {
    /// The command line, for a report that names what was run.
    pub(crate) fn describe(&self) -> String {
        if self.args.is_empty() {
            return self.program.clone();
        }
        format!("{} {}", self.program, self.args.join(" "))
    }

    /// True when the deadline expired rather than the command failing.
    pub(crate) fn is_timeout(&self) -> bool {
        self.kind == HostCommandFailure::TimedOut
    }
}

/// The result of a host command that completed within its deadline.
#[derive(Debug)]
pub(crate) struct HostOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

impl HostOutput {
    pub(crate) fn success(&self) -> bool {
        self.status.success()
    }

    /// stdout as text, lossily decoded because command output is diagnostic.
    pub(crate) fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr as text, trimmed of the trailing newline commands usually add.
    pub(crate) fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// One host command, with its deadline and output cap.
#[derive(Debug)]
pub(crate) struct HostCommand {
    program: OsString,
    args: Vec<OsString>,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    output_cap: usize,
    capture_output: bool,
    /// Network namespace the child joins before exec, given as a pid.
    ///
    /// nsenter exists to do exactly this, and it costs a process on the workspace
    /// start path. Joining the namespace in the child's own pre-exec hook does the
    /// same thing with no extra process, and keeps the deadline and output
    /// handling identical to every other host command.
    netns_pid: Option<u32>,
}

impl HostCommand {
    pub(crate) fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            stdin: None,
            timeout: configured_timeout(),
            output_cap: DEFAULT_OUTPUT_CAP,
            capture_output: true,
            netns_pid: None,
        }
    }

    /// Run the command inside the network namespace of this pid.
    pub(crate) fn netns(mut self, pid: u32) -> Self {
        self.netns_pid = Some(pid);
        self
    }

    /// Discard stdout and stderr instead of capturing them.
    ///
    /// Probe and check commands run on the workspace start path and their output
    /// is never read. Skipping the pipes keeps those calls to one spawn.
    pub(crate) fn discard_output(mut self) -> Self {
        self.capture_output = false;
        self
    }

    pub(crate) fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub(crate) fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    /// Feed `bytes` to the command standard input.
    pub(crate) fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    /// Override the deadline, clamped to MAX_TIMEOUT.
    pub(crate) fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.min(MAX_TIMEOUT);
        self
    }

    pub(crate) fn output_cap(mut self, cap: usize) -> Self {
        self.output_cap = cap;
        self
    }

    /// The command line, for error reports and tracing.
    pub(crate) fn describe(&self) -> String {
        let program = self.program.to_string_lossy();
        if self.args.is_empty() {
            return program.into_owned();
        }
        let args = self
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        format!("{program} {args}")
    }

    /// Run the command, stopping it if it outlives its deadline.
    ///
    /// The child is placed in its own process group so a timeout stops the whole
    /// tree rather than only the process that was started.
    pub(crate) fn run(mut self) -> Result<HostOutput> {
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

/// The deadline for a host command that does not set its own.
///
/// Operators can raise it for a slow host, but never past MAX_TIMEOUT.
/// Join the network namespace of another process, from the child before exec.
///
/// This runs between fork and exec, so it may only call async-signal-safe
/// functions: open, setns, and close qualify. Allocation and any Rust-level error
/// formatting do not, which is why the failure is returned as a raw errno.
fn enter_network_namespace(pid: u32) -> std::io::Result<()> {
    let mut path = [0u8; 32];
    let mut length = 0usize;
    for byte in b"/proc/" {
        path[length] = *byte;
        length += 1;
    }
    // Render the pid without formatting machinery.
    let mut digits = [0u8; 10];
    let mut digit_count = 0usize;
    let mut value = pid;
    if value == 0 {
        digits[0] = b'0';
        digit_count = 1;
    } else {
        while value > 0 {
            digits[digit_count] = b'0' + (value % 10) as u8;
            digit_count += 1;
            value /= 10;
        }
    }
    for index in (0..digit_count).rev() {
        path[length] = digits[index];
        length += 1;
    }
    for byte in b"/ns/net" {
        path[length] = *byte;
        length += 1;
    }
    path[length] = 0;

    let fd = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let result = unsafe { libc::setns(fd, libc::CLONE_NEWNET) };
    let error = std::io::Error::last_os_error();
    unsafe { libc::close(fd) };
    if result != 0 {
        return Err(error);
    }
    Ok(())
}

fn configured_timeout() -> Duration {
    match std::env::var("ENCLAVE_HOST_COMMAND_TIMEOUT_SECS") {
        Ok(value) => match value.parse::<u64>() {
            Ok(seconds) if seconds > 0 => Duration::from_secs(seconds).min(MAX_TIMEOUT),
            _ => {
                tracing::warn!(
                    "ignoring ENCLAVE_HOST_COMMAND_TIMEOUT_SECS value {value}; using {:?}",
                    DEFAULT_TIMEOUT
                );
                DEFAULT_TIMEOUT
            }
        },
        Err(_) => DEFAULT_TIMEOUT,
    }
}

/// Resolve a program the way a shell would, without starting one.
///
/// Capability probes run on every daemon start and every quota-backed workspace
/// start. Answering them from the filesystem avoids a fork per probe and keeps
/// the answer stable for the life of the process.
pub(crate) fn command_on_path(program: &str) -> Option<std::path::PathBuf> {
    let candidate = std::path::Path::new(program);
    if candidate.is_absolute() {
        return is_executable(candidate).then(|| candidate.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let candidate = directory.join(program);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// True when the error is a host-command deadline that expired.
pub(crate) fn is_timeout(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<HostCommandError>()
        .is_some_and(HostCommandError::is_timeout)
}

#[cfg(test)]
#[path = "../../tests/src/hostcmd.rs"]
mod tests;
