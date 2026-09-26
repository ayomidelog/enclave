//! Running a host program while capturing its output and its log.
//!
//! A bootstrap program runs for minutes and its log is what an operator reads
//! when it fails, so the output is streamed to the log as it arrives rather
//! than buffered until the process exits.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};

pub(crate) fn run_command_with_live_log(
    command: &mut Command,
    log_path: &Path,
    label: &str,
) -> Result<Output> {
    let log_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(log_path)
        .with_context(|| format!("failed to open {} log {}", label, log_path.display()))?;
    let log_writer = Arc::new(Mutex::new(log_file));
    {
        let mut log = log_writer
            .lock()
            .map_err(|_| anyhow!("failed to lock {} log writer", label))?;
        writeln!(log, "# {} live log", label)?;
        writeln!(log)?;
    }

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {}", label))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture {} stdout", label))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture {} stderr", label))?;

    let stdout_handle = pump_command_stream(stdout, "stdout", Arc::clone(&log_writer));
    let stderr_handle = pump_command_stream(stderr, "stderr", Arc::clone(&log_writer));

    let status = child
        .wait()
        .with_context(|| format!("failed waiting for {}", label))?;
    let stdout = join_stream_capture(stdout_handle, label, "stdout")?;
    let stderr = join_stream_capture(stderr_handle, label, "stderr")?;

    {
        let mut log = log_writer
            .lock()
            .map_err(|_| anyhow!("failed to lock {} log writer", label))?;
        writeln!(log)?;
        writeln!(log, "# {} exit status: {}", label, status)?;
        log.flush()?;
    }

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn pump_command_stream<R>(
    reader: R,
    stream_label: &'static str,
    log_writer: Arc<Mutex<fs::File>>,
) -> thread::JoinHandle<Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut captured = Vec::new();

        loop {
            let mut line = Vec::new();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }

            captured.extend_from_slice(&line);
            let mut log = log_writer
                .lock()
                .map_err(|_| anyhow!("failed to lock command log writer"))?;
            log.write_all(format!("[{}] ", stream_label).as_bytes())?;
            log.write_all(&line)?;
            if !line.ends_with(b"\n") {
                log.write_all(b"\n")?;
            }
            log.flush()?;
        }

        Ok(captured)
    })
}

fn join_stream_capture(
    handle: thread::JoinHandle<Result<Vec<u8>>>,
    label: &str,
    stream_label: &str,
) -> Result<Vec<u8>> {
    match handle.join() {
        Ok(result) => {
            result.with_context(|| format!("failed to capture {} {}", label, stream_label))
        }
        Err(_) => bail!("{} {} stream thread panicked", label, stream_label),
    }
}

pub(crate) fn command_failure_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }

    "debootstrap did not return stderr/stdout".to_string()
}
