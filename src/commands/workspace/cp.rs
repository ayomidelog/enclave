use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::cli::WorkspaceCpArgs;
use crate::workspace::WorkspaceCpResult;

use super::super::send_managed;
use super::WorkspaceCommandContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathSide {
    Host,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TransferPath {
    side: PathSide,
    path: String,
}

pub(super) fn run_workspace_cp(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceCpArgs,
) -> Result<()> {
    let (src, dst) = parse_transfer_paths(&args.src, &args.dst)?;
    let request = json!({
        "sandbox": args.sandbox,
        "workspace": args.workspace,
        "src": src.path,
        "dst": dst.path,
        "direction": match src.side {
            PathSide::Host => "host_to_workspace",
            PathSide::Workspace => "workspace_to_host",
        },
        "gzip": args.gzip,
    });
    let response = if args.progress {
        send_with_progress(ctx.socket, request)?
    } else {
        send_managed(ctx.socket, "workspace.cp", request)?
    };
    let result: WorkspaceCpResult = serde_json::from_value(response)?;
    println!(
        "copied {} logical bytes in {:.3}s",
        result.logical_bytes,
        result.elapsed_ms as f64 / 1000.0
    );
    Ok(())
}

fn send_with_progress(socket: &Path, request: serde_json::Value) -> Result<serde_json::Value> {
    let stopped = Arc::new(AtomicBool::new(false));
    let progress_stopped = Arc::clone(&stopped);
    let progress_thread = thread::spawn(move || {
        let started = std::time::Instant::now();
        while !progress_stopped.load(Ordering::Relaxed) {
            eprintln!(
                "workspace cp: elapsed {:.1}s",
                started.elapsed().as_secs_f64()
            );
            thread::sleep(Duration::from_millis(500));
        }
    });
    let response = send_managed(socket, "workspace.cp", request);
    stopped.store(true, Ordering::Relaxed);
    let _ = progress_thread.join();
    response
}

fn parse_transfer_paths(src: &str, dst: &str) -> Result<(TransferPath, TransferPath)> {
    let src = parse_transfer_path(src)?;
    let dst = parse_transfer_path(dst)?;
    if src.side == dst.side {
        bail!(
            "workspace cp requires exactly one workspace path prefixed with 'ws:/'; got {} and {}",
            describe_side(src.side),
            describe_side(dst.side)
        );
    }

    let mut src = src;
    let mut dst = dst;
    if src.side == PathSide::Host {
        src.path = absolute_host_path(&src.path)?;
    } else {
        validate_workspace_path(&src.path)?;
    }
    if dst.side == PathSide::Host {
        dst.path = absolute_host_path(&dst.path)?;
    } else {
        validate_workspace_path(&dst.path)?;
    }
    Ok((src, dst))
}

fn parse_transfer_path(raw: &str) -> Result<TransferPath> {
    if let Some(path) = raw.strip_prefix("ws:/") {
        let path = format!("/{path}");
        return Ok(TransferPath {
            side: PathSide::Workspace,
            path,
        });
    }
    if raw.starts_with("ws:") {
        bail!("workspace paths must use the 'ws:/' prefix: {raw}");
    }
    Ok(TransferPath {
        side: PathSide::Host,
        path: raw.to_string(),
    })
}

fn validate_workspace_path(path: &str) -> Result<()> {
    let path = Path::new(path);
    if !path.is_absolute() {
        bail!("workspace path must be absolute and use the 'ws:/' prefix");
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("workspace path cannot contain '..': {}", path.display());
    }
    Ok(())
}

fn absolute_host_path(path: &str) -> Result<String> {
    let path = PathBuf::from(path);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(absolute
        .to_str()
        .context("host path is not valid UTF-8")?
        .to_string())
}

fn describe_side(side: PathSide) -> &'static str {
    match side {
        PathSide::Host => "host path",
        PathSide::Workspace => "workspace path",
    }
}

#[cfg(test)]
#[path = "../../../tests/src/commands/workspace/cp.rs"]
mod tests;
