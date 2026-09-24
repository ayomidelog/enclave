use super::*;

pub(super) fn run_workspace_logs(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceLogsArgs,
) -> Result<()> {
    let (sandbox, workspace) =
        resolve_workspace_target_from_optional(ctx, &args.target, args.workspace.as_deref())?;
    let mut logs = fetch_workspace_logs(ctx, &sandbox, &workspace, args.tail)?;
    if logs.content.is_empty() {
        println!("no logs");
    } else {
        print!("{}", logs.content);
        std::io::stdout().flush()?;
    }
    if !args.follow {
        return Ok(());
    }

    let mut offset = logs.next_offset;
    let mut stream_id = logs.stream_id;
    let mut poll_interval = LOG_FOLLOW_POLL_INTERVAL;
    loop {
        thread::sleep(poll_interval);
        logs = fetch_workspace_logs_at_offset(
            ctx,
            &sandbox,
            &workspace,
            offset,
            stream_id.as_deref(),
        )?;
        if logs.content.is_empty() && !logs.reset {
            poll_interval = std::cmp::min(
                poll_interval.saturating_mul(2),
                LOG_FOLLOW_MAX_POLL_INTERVAL,
            );
            continue;
        }
        poll_interval = LOG_FOLLOW_POLL_INTERVAL;
        if logs.reset {
            print!("\n[enclave] log stream reset; showing current log content\n");
        }
        print!("{}", logs.content);
        std::io::stdout().flush()?;
        offset = logs.next_offset;
        stream_id = logs.stream_id;
    }
}

fn fetch_workspace_logs(
    ctx: &WorkspaceCommandContext<'_>,
    sandbox: &str,
    workspace: &str,
    tail: Option<usize>,
) -> Result<WorkspaceLogsResult> {
    let response = send_managed(
        ctx.socket,
        "workspace.logs",
        json!({
            "sandbox": sandbox,
            "workspace": workspace,
            "tail": tail,
        }),
    )?;
    serde_json::from_value(response).map_err(Into::into)
}

fn fetch_workspace_logs_at_offset(
    ctx: &WorkspaceCommandContext<'_>,
    sandbox: &str,
    workspace: &str,
    offset: u64,
    stream_id: Option<&str>,
) -> Result<WorkspaceLogsResult> {
    let response = send_managed(
        ctx.socket,
        "workspace.logs",
        json!({
            "sandbox": sandbox,
            "workspace": workspace,
            "offset": offset,
            "stream_id": stream_id,
        }),
    )?;
    serde_json::from_value(response).map_err(Into::into)
}
