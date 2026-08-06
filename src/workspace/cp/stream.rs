use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::path::DestinationPlan;
use crate::workspace::exec::spawn_workspace_command;
use crate::workspace::types::WorkspaceMetadata;

#[derive(Debug)]
pub(super) struct TransferOutput {
    pub(super) logical_bytes: u64,
}

pub(super) struct ChildGuard {
    child: Child,
    armed: bool,
}

impl ChildGuard {
    pub(super) fn new(child: Child) -> Self {
        Self { child, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub(super) fn workspace_path_is_directory(
    workspace: &WorkspaceMetadata,
    path: &str,
    client_stream: Option<&UnixStream>,
) -> Result<bool> {
    let output = run_workspace_utility(workspace, &["test", "-d", path], client_stream)?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    bail!(
        "failed to inspect workspace destination '{}': {}",
        path,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(super) fn workspace_path_has_symlink(
    workspace: &WorkspaceMetadata,
    path: &str,
    client_stream: Option<&UnixStream>,
) -> Result<()> {
    let mut prefix = std::path::PathBuf::new();
    for component in Path::new(path).components() {
        prefix.push(component.as_os_str());
        if prefix == Path::new("/") {
            continue;
        }
        let output = run_workspace_utility(
            workspace,
            &["test", "-L", &prefix.to_string_lossy()],
            client_stream,
        )?;
        if output.status.success() {
            bail!(
                "refusing to copy through symlinked workspace destination component '{}'",
                prefix.display()
            );
        }
    }
    Ok(())
}

pub(super) fn run_host_to_workspace(
    workspace: &WorkspaceMetadata,
    source: &str,
    destination: &DestinationPlan,
    source_name: &str,
    client_stream: Option<&UnixStream>,
) -> Result<TransferOutput> {
    let mut host_tar =
        ChildGuard::new(spawn_tar(tar_create_args(source, source_name), true, None)?);
    let stream = host_tar
        .child
        .stdout
        .take()
        .context("host tar stdout unavailable")?;
    let workspace_tar = spawn_workspace_command(
        workspace,
        "/home",
        &tar_extract_args(destination),
        Stdio::from(stream),
        Stdio::null(),
        Stdio::piped(),
    );
    let mut workspace_tar = match workspace_tar {
        Ok(child) => ChildGuard::new(child),
        Err(error) => {
            return Err(error);
        }
    };

    let workspace_output = wait_child_output(&mut workspace_tar, client_stream)
        .context("failed to run workspace tar extractor");
    let host_output =
        wait_child_output(&mut host_tar, client_stream).context("failed to wait for host tar");
    let workspace_output = workspace_output?;
    let host_output = host_output?;
    ensure_transfer_success("host tar", &host_output)?;
    ensure_transfer_success("workspace tar", &workspace_output)?;
    if destination.rename_to.is_some() {
        move_workspace_destination(workspace, destination, source_name, client_stream)?;
    }
    Ok(TransferOutput {
        logical_bytes: path_size(Path::new(source)).unwrap_or(0),
    })
}

pub(super) fn run_workspace_to_host(
    workspace: &WorkspaceMetadata,
    source: &str,
    destination: &DestinationPlan,
    source_name: &str,
    client_stream: Option<&UnixStream>,
) -> Result<TransferOutput> {
    let mut workspace_tar = ChildGuard::new(spawn_workspace_command(
        workspace,
        "/home",
        &tar_create_args(source, source_name),
        Stdio::null(),
        Stdio::piped(),
        Stdio::piped(),
    )?);
    let stream = workspace_tar
        .child
        .stdout
        .take()
        .context("workspace tar stdout unavailable")?;
    let host_tar = spawn_tar(
        tar_extract_args(destination),
        false,
        Some(Stdio::from(stream)),
    );
    let mut host_tar = match host_tar {
        Ok(child) => ChildGuard::new(child),
        Err(error) => {
            return Err(error);
        }
    };
    let host_output = wait_child_output(&mut host_tar, client_stream)
        .context("failed to wait for host tar extractor");
    let workspace_output = wait_child_output(&mut workspace_tar, client_stream)
        .context("failed to wait for workspace tar");
    let host_output = host_output?;
    let workspace_output = workspace_output?;
    ensure_transfer_success("workspace tar", &workspace_output)?;
    ensure_transfer_success("host tar", &host_output)?;
    if destination.rename_to.is_some() {
        move_host_destination(destination, source_name)?;
    }
    Ok(TransferOutput {
        logical_bytes: path_size(&destination.final_path(source_name)).unwrap_or(0),
    })
}

fn spawn_tar(args: Vec<String>, creating: bool, input: Option<Stdio>) -> Result<Child> {
    let mut command = Command::new("tar");
    let extracting = !creating;
    command
        .args(args)
        .stdin(if extracting {
            input.unwrap_or_else(Stdio::piped)
        } else {
            Stdio::null()
        })
        .stdout(if creating {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start tar")
}

fn tar_create_args(source: &str, source_name: &str) -> Vec<String> {
    let parent = Path::new(source)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("/"));
    vec![
        "-C".into(),
        parent.to_string_lossy().into_owned(),
        "-cf".into(),
        "-".into(),
        "--".into(),
        source_name.into(),
    ]
}

fn tar_extract_args(destination: &DestinationPlan) -> Vec<String> {
    let mut args = vec![
        "-C".into(),
        destination.parent.to_string_lossy().into_owned(),
        "-xpf".into(),
        "-".into(),
        "-o".into(),
    ];
    args.push("--".into());
    args
}

fn move_workspace_destination(
    workspace: &WorkspaceMetadata,
    destination: &DestinationPlan,
    source_name: &str,
    client_stream: Option<&UnixStream>,
) -> Result<()> {
    let source = destination.extracted_path(source_name);
    let target = destination.final_path(source_name);
    let output = run_workspace_utility(
        workspace,
        &[
            "mv",
            "--",
            &source.to_string_lossy(),
            &target.to_string_lossy(),
        ],
        client_stream,
    )?;
    ensure_transfer_success("workspace mv", &output)
}

fn move_host_destination(destination: &DestinationPlan, source_name: &str) -> Result<()> {
    let source = destination.extracted_path(source_name);
    let target = destination.final_path(source_name);
    let output = Command::new("mv")
        .args(["--", &source.to_string_lossy(), &target.to_string_lossy()])
        .output()
        .context("failed to rename host copy destination")?;
    ensure_transfer_success("host mv", &output)
}

fn path_size(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        total = total.saturating_add(path_size(&entry?.path())?);
    }
    Ok(total)
}

fn ensure_transfer_success(label: &str, output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{} failed (exit {}): {}",
        label,
        output
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |code| code.to_string()),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn run_workspace_utility(
    workspace: &WorkspaceMetadata,
    command: &[&str],
    client_stream: Option<&UnixStream>,
) -> Result<Output> {
    let primary = spawn_workspace_command(
        workspace,
        "/home",
        &command
            .iter()
            .map(|item| (*item).to_string())
            .collect::<Vec<_>>(),
        Stdio::null(),
        Stdio::null(),
        Stdio::piped(),
    )?;
    let mut primary = ChildGuard::new(primary);
    let output = wait_child_output(&mut primary, client_stream)?;
    if output.status.code() != Some(127) {
        return Ok(output);
    }

    let mut fallback_command = vec!["/bin/busybox".to_string()];
    fallback_command.extend(command.iter().map(|item| (*item).to_string()));
    let fallback = spawn_workspace_command(
        workspace,
        "/home",
        &fallback_command,
        Stdio::null(),
        Stdio::null(),
        Stdio::piped(),
    )?;
    let mut fallback = ChildGuard::new(fallback);
    wait_child_output(&mut fallback, client_stream)
}

pub(super) fn wait_child_output(
    child: &mut ChildGuard,
    client_stream: Option<&UnixStream>,
) -> Result<Output> {
    let stderr_reader = child.child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut stderr = Vec::new();
            pipe.read_to_end(&mut stderr).map(|_| stderr)
        })
    });
    let status = loop {
        match child
            .child
            .try_wait()
            .context("failed to poll child process")?
        {
            Some(status) => break status,
            None => {
                if client_stream.is_some_and(|stream| !client_is_connected(stream)) {
                    let _ = child.child.kill();
                    let _ = child.child.wait();
                    if let Some(reader) = stderr_reader {
                        let _ = reader.join();
                    }
                    bail!("workspace cp client disconnected; transfer cancelled")
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let stderr = match stderr_reader {
        Some(reader) => reader
            .join()
            .map_err(|_| anyhow::anyhow!("child stderr reader panicked"))??,
        None => Vec::new(),
    };
    child.disarm();
    Ok(Output {
        status,
        stdout: Vec::new(),
        stderr,
    })
}

fn client_is_connected(stream: &UnixStream) -> bool {
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return true;
    }
    descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0
}
