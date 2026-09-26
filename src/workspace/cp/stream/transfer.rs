use super::*;

pub(crate) fn run_host_to_workspace(
    workspace: &WorkspaceMetadata,
    source: &str,
    destination: &DestinationPlan,
    source_name: &str,
    logical_bytes: u64,
    gzip: bool,
    client_stream: Option<&UnixStream>,
) -> Result<TransferOutput> {
    let stage =
        create_workspace_staging_directory(workspace, destination, source_name, client_stream)?;
    let result = if is_regular_file(source)? {
        run_host_regular_file_to_workspace(
            workspace,
            source,
            destination,
            source_name,
            logical_bytes,
            &stage,
            client_stream,
        )
    } else {
        run_host_archive_to_workspace(
            workspace,
            source,
            destination,
            source_name,
            &stage,
            TransferContext {
                gzip,
                client_stream,
            },
        )
    };
    remove_workspace_staging_directory(workspace, &stage);
    result
}

pub(crate) fn run_host_regular_file_to_workspace(
    workspace: &WorkspaceMetadata,
    source: &str,
    destination: &DestinationPlan,
    source_name: &str,
    logical_bytes: u64,
    stage: &str,
    client_stream: Option<&UnixStream>,
) -> Result<TransferOutput> {
    let source_metadata =
        fs::metadata(source).with_context(|| format!("failed to stat host source '{}'", source))?;
    let target = format!("{stage}/{source_name}");
    let mut workspace_writer = ChildGuard::new(spawn_workspace_file_receiver(
        workspace,
        &target,
        Stdio::piped(),
        Stdio::piped(),
    )?);
    let stdin = workspace_writer
        .child
        .stdin
        .take()
        .context("workspace direct-copy stdin unavailable")?;
    set_pipe_capacity(stdin.as_raw_fd());
    let mut source_file =
        File::open(source).with_context(|| format!("failed to open host source '{}'", source))?;
    sendfile_to_pipe(&mut source_file, stdin.as_raw_fd(), logical_bytes)?;
    drop(stdin);
    let workspace_output = wait_child_output(&mut workspace_writer, client_stream)
        .context("failed to run workspace direct file writer")?;
    ensure_transfer_success("workspace direct file writer", &workspace_output)?;
    restore_workspace_metadata(workspace, &target, &source_metadata)?;
    move_workspace_path(
        workspace,
        &target,
        &destination.final_path(source_name).to_string_lossy(),
        client_stream,
    )?;
    Ok(TransferOutput {
        logical_bytes,
        files: 1,
    })
}

pub(crate) fn is_regular_file(path: &str) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("host source '{}' does not exist", path))?;
    Ok(metadata.is_file() && !metadata.file_type().is_symlink())
}

pub(crate) fn sendfile_to_pipe(
    source: &mut File,
    destination_fd: i32,
    expected_bytes: u64,
) -> Result<()> {
    if splice_to_pipe(source, destination_fd, expected_bytes)? {
        return Ok(());
    }
    let mut transferred = 0u64;
    while transferred < expected_bytes {
        let remaining = expected_bytes - transferred;
        let amount = unsafe {
            libc::sendfile(
                destination_fd,
                source.as_raw_fd(),
                std::ptr::null_mut(),
                remaining.min(4 * 1024 * 1024) as usize,
            )
        };
        if amount > 0 {
            transferred = transferred.saturating_add(amount as u64);
            continue;
        }
        if amount == 0 {
            bail!(
                "host source ended before expected size ({} of {} bytes)",
                transferred,
                expected_bytes
            );
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error).context("failed to stream host file with sendfile");
    }
    Ok(())
}

pub(crate) fn splice_to_pipe(
    source: &File,
    destination_fd: i32,
    expected_bytes: u64,
) -> Result<bool> {
    let mut transferred = 0u64;
    while transferred < expected_bytes {
        let amount = unsafe {
            libc::splice(
                source.as_raw_fd(),
                std::ptr::null_mut(),
                destination_fd,
                std::ptr::null_mut(),
                (expected_bytes - transferred).min(4 * 1024 * 1024) as usize,
                0,
            )
        };
        if amount > 0 {
            transferred = transferred.saturating_add(amount as u64);
            continue;
        }
        if amount == 0 {
            bail!(
                "host source ended before expected size ({} of {} bytes)",
                transferred,
                expected_bytes
            );
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if matches!(
            error.raw_os_error(),
            Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)
        ) {
            return Ok(false);
        }
        return Err(error).context("failed to stream host file with splice");
    }
    Ok(true)
}

pub(crate) fn run_host_archive_to_workspace(
    workspace: &WorkspaceMetadata,
    source: &str,
    destination: &DestinationPlan,
    source_name: &str,
    stage: &str,
    context: TransferContext<'_>,
) -> Result<TransferOutput> {
    let source_metadata =
        fs::metadata(source).with_context(|| format!("failed to stat host source '{}'", source))?;
    let mut workspace_tar = ChildGuard::new(spawn_workspace_command(
        workspace,
        "/home",
        &workspace_tar_command(tar_extract_args_at(stage, context.gzip)),
        Stdio::piped(),
        Stdio::null(),
        Stdio::piped(),
    )?);
    let mut stdin = workspace_tar
        .child
        .stdin
        .take()
        .context("workspace tar stdin unavailable")?;
    set_pipe_capacity(stdin.as_raw_fd());
    let (logical_bytes, files) = if source_metadata.is_dir() {
        if context.gzip {
            let mut encoder = GzEncoder::new(stdin, Compression::default());
            let stats = write_host_directory_archive(source, source_name, &mut encoder)?;
            encoder.finish().context("failed to finish gzip stream")?;
            stats
        } else {
            let stats = write_host_directory_archive(source, source_name, &mut stdin)?;
            drop(stdin);
            stats
        }
    } else {
        let mut archive = tar::Builder::new(stdin);
        archive
            .append_path_with_name(source, source_name)
            .with_context(|| format!("failed to archive host source '{}'", source))?;
        archive
            .finish()
            .context("failed to finish host tar archive")?;
        drop(archive);
        (source_metadata.len(), 1)
    };
    let workspace_output = wait_child_output(&mut workspace_tar, context.client_stream)
        .context("failed to run workspace tar extractor")?;
    ensure_transfer_success("workspace tar", &workspace_output)?;
    if source_metadata.is_file() {
        restore_workspace_metadata(
            workspace,
            &format!("{stage}/{source_name}"),
            &source_metadata,
        )?;
    }
    move_workspace_path(
        workspace,
        &format!("{stage}/{source_name}"),
        &destination.final_path(source_name).to_string_lossy(),
        context.client_stream,
    )?;
    Ok(TransferOutput {
        logical_bytes,
        files,
    })
}

pub(crate) fn run_workspace_to_host(
    workspace: &WorkspaceMetadata,
    source: &str,
    destination: &DestinationPlan,
    source_name: &str,
    gzip: bool,
    client_stream: Option<&UnixStream>,
) -> Result<TransferOutput> {
    let stage = HostStagingDirectory::create(&destination.parent)?;
    let mut workspace_tar = ChildGuard::new(spawn_workspace_command(
        workspace,
        "/home",
        &workspace_tar_command(tar_create_args(source, source_name, gzip)),
        Stdio::null(),
        Stdio::piped(),
        Stdio::piped(),
    )?);
    let stream = workspace_tar
        .child
        .stdout
        .take()
        .context("workspace tar stdout unavailable")?;
    set_pipe_capacity(stream.as_raw_fd());
    let (logical_bytes, files) = if gzip {
        extract_workspace_archive_with_stats(GzDecoder::new(stream), &stage, source_name)
    } else {
        extract_workspace_archive_with_stats(stream, &stage, source_name)
    }
    .context("failed to validate workspace tar archive")?;
    let workspace_output = wait_child_output(&mut workspace_tar, client_stream)
        .context("failed to wait for workspace tar")?;
    ensure_transfer_success("workspace tar", &workspace_output)?;
    stage.commit(source_name, destination.final_name(source_name))?;
    Ok(TransferOutput {
        logical_bytes,
        files,
    })
}
