use super::*;

pub(crate) fn workspace_path_is_directory(
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

pub(crate) fn validate_workspace_source(
    workspace: &WorkspaceMetadata,
    path: &str,
    client_stream: Option<&UnixStream>,
) -> Result<()> {
    let output = run_workspace_utility(
        workspace,
        &[
            "sh",
            "-c",
            "unsupported=$(find \"$1\" \\( -type p -o -type b -o -type c -o -type s \\) -print -quit) || exit 2; test -z \"$unsupported\"",
            "sh",
            path,
        ],
        client_stream,
    )?;
    if output.status.success() {
        return Ok(());
    }
    if output.status.code() == Some(1) {
        bail!(
            "refusing to copy workspace source '{}' with special files",
            path
        );
    }
    bail!(
        "failed to validate workspace source '{}': {}",
        path,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(crate) fn workspace_path_has_symlink(
    workspace: &WorkspaceMetadata,
    path: &str,
    client_stream: Option<&UnixStream>,
) -> Result<()> {
    let mut prefix = PathBuf::new();
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

pub(crate) fn create_workspace_staging_directory(
    workspace: &WorkspaceMetadata,
    destination: &DestinationPlan,
    source_name: &str,
    client_stream: Option<&UnixStream>,
) -> Result<String> {
    let target = destination.final_path(source_name);
    if workspace_path_exists(workspace, &target.to_string_lossy(), client_stream)? {
        bail!(
            "workspace destination '{}' already exists",
            target.display()
        );
    }
    let stage = destination
        .parent
        .join(format!(".enclave-cp-{}", Uuid::new_v4()))
        .to_string_lossy()
        .to_string();
    let output = run_workspace_utility(workspace, &["mkdir", "--", &stage], client_stream)?;
    ensure_transfer_success("workspace staging directory", &output)?;
    Ok(stage)
}

pub(crate) fn remove_workspace_staging_directory(workspace: &WorkspaceMetadata, stage: &str) {
    let _ = run_workspace_utility(workspace, &["rm", "-rf", "--", stage], None);
}

pub(crate) fn workspace_path_exists(
    workspace: &WorkspaceMetadata,
    path: &str,
    client_stream: Option<&UnixStream>,
) -> Result<bool> {
    let output = run_workspace_utility(workspace, &["test", "-e", path], client_stream)?;
    if output.status.success() {
        return Ok(true);
    }
    let symlink = run_workspace_utility(workspace, &["test", "-L", path], client_stream)?;
    if symlink.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) && symlink.status.code() == Some(1) {
        return Ok(false);
    }
    bail!("failed to inspect workspace destination '{}'", path)
}

pub(crate) fn move_workspace_path(
    workspace: &WorkspaceMetadata,
    source: &str,
    target: &str,
    client_stream: Option<&UnixStream>,
) -> Result<()> {
    let output = run_workspace_utility(workspace, &["mv", "--", source, target], client_stream)?;
    ensure_transfer_success("workspace staged move", &output)
}
