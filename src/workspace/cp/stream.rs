use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::{self, File};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::Archive;
use uuid::Uuid;

use super::path::DestinationPlan;
use crate::workspace::exec::{spawn_workspace_command, spawn_workspace_file_receiver};
use crate::workspace::types::WorkspaceMetadata;

struct TransferContext<'a> {
    gzip: bool,
    client_stream: Option<&'a UnixStream>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct UtilityCacheKey {
    sandbox_id: String,
    workspace_id: String,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    command: String,
}

static UTILITY_CACHE: OnceLock<Mutex<HashMap<UtilityCacheKey, bool>>> = OnceLock::new();
const UTILITY_CACHE_LIMIT: usize = 512;

#[derive(Debug)]
pub(super) struct TransferOutput {
    pub(super) logical_bytes: u64,
    pub(super) files: u64,
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

pub(super) struct HostStagingDirectory {
    parent: File,
    stage_name: String,
    stage_path: PathBuf,
    active: bool,
}

impl HostStagingDirectory {
    pub(super) fn create(parent: &Path) -> Result<Self> {
        let parent = open_host_directory(parent)?;
        for _ in 0..16 {
            let stage_name = format!(".enclave-cp-{}", Uuid::new_v4());
            let name = cstring(&stage_name)?;
            let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
            if result == 0 {
                let stage_path = PathBuf::from("/proc/self/fd")
                    .join(parent.as_raw_fd().to_string())
                    .join(&stage_name);
                return Ok(Self {
                    parent,
                    stage_name,
                    stage_path,
                    active: true,
                });
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error).context("failed to create host transfer staging directory");
            }
        }
        bail!("failed to allocate a unique host transfer staging directory")
    }

    pub(super) fn path(&self) -> &Path {
        &self.stage_path
    }

    pub(super) fn commit(mut self, source_name: &str, target_name: &str) -> Result<()> {
        ensure_host_entry_absent(&self.parent, target_name)?;
        let source = cstring(&format!("{}/{}", self.stage_name, source_name))?;
        let target = cstring(target_name)?;
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.parent.as_raw_fd(),
                source.as_ptr(),
                self.parent.as_raw_fd(),
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to commit staged host transfer '{}' as '{}'",
                    source_name, target_name
                )
            });
        }
        self.active = false;
        let _ = fs::remove_dir(&self.stage_path);
        Ok(())
    }
}

impl Drop for HostStagingDirectory {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_dir_all(&self.stage_path);
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

pub(super) fn validate_workspace_source(
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

pub(super) fn workspace_path_has_symlink(
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

pub(super) fn run_host_to_workspace(
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
            logical_bytes,
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

fn run_host_regular_file_to_workspace(
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

fn is_regular_file(path: &str) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("host source '{}' does not exist", path))?;
    Ok(metadata.is_file() && !metadata.file_type().is_symlink())
}

fn sendfile_to_pipe(source: &mut File, destination_fd: i32, expected_bytes: u64) -> Result<()> {
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

fn splice_to_pipe(source: &File, destination_fd: i32, expected_bytes: u64) -> Result<bool> {
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

fn run_host_archive_to_workspace(
    workspace: &WorkspaceMetadata,
    source: &str,
    destination: &DestinationPlan,
    source_name: &str,
    logical_bytes: u64,
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
    if source_metadata.is_dir() {
        let parent = Path::new(source)
            .parent()
            .context("host directory source has no parent")?;
        let mut host_tar = ChildGuard::new(spawn_host_tar(parent, source_name)?);
        let mut host_stdout = host_tar
            .child
            .stdout
            .take()
            .context("host tar stdout unavailable")?;
        if context.gzip {
            let mut encoder = GzEncoder::new(stdin, Compression::default());
            std::io::copy(&mut host_stdout, &mut encoder)
                .with_context(|| format!("failed to compress host tar for '{}'", source))?;
            stdin = encoder.finish().context("failed to finish gzip stream")?;
        } else {
            std::io::copy(&mut host_stdout, &mut stdin)
                .with_context(|| format!("failed to stream host tar for '{}'", source))?;
        }
        drop(host_stdout);
        drop(stdin);
        let status = host_tar
            .child
            .wait()
            .context("failed to wait for host tar")?;
        host_tar.disarm();
        if !status.success() {
            bail!("host tar failed for '{}' with status {}", source, status);
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
    }
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
        files: if source_metadata.is_dir() { 0 } else { 1 },
    })
}

fn spawn_host_tar(parent: &Path, source_name: &str) -> Result<Child> {
    crate::perf::record_process_spawn();
    Command::new("tar")
        .args([
            "-C",
            parent
                .to_str()
                .context("host tar parent path is not valid UTF-8")?,
            "-cf",
            "-",
            "--",
            source_name,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start host tar")
}

fn restore_workspace_metadata(
    workspace: &WorkspaceMetadata,
    path: &str,
    metadata: &fs::Metadata,
) -> Result<()> {
    let relative = Path::new(path)
        .strip_prefix("/home")
        .context("workspace staging path is outside /home")?;
    let base = workspace
        .home_mount_source_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&workspace.filesystem_path));
    let target = base.join(relative);
    fs::set_permissions(
        &target,
        fs::Permissions::from_mode(metadata.permissions().mode()),
    )
    .with_context(|| format!("failed to restore workspace permissions {}", path))?;
    let accessed = metadata
        .accessed()
        .context("failed to read host source access time")?
        .duration_since(std::time::UNIX_EPOCH)
        .context("host source access time predates Unix epoch")?;
    let modified = metadata
        .modified()
        .context("failed to read host source modification time")?
        .duration_since(std::time::UNIX_EPOCH)
        .context("host source modification time predates Unix epoch")?;
    let target = CString::new(target.as_os_str().as_bytes())
        .context("workspace staging path contains an interior NUL byte")?;
    let times = [
        libc::timespec {
            tv_sec: accessed.as_secs() as libc::time_t,
            tv_nsec: accessed.subsec_nanos() as libc::c_long,
        },
        libc::timespec {
            tv_sec: modified.as_secs() as libc::time_t,
            tv_nsec: modified.subsec_nanos() as libc::c_long,
        },
    ];
    let result = unsafe { libc::utimensat(libc::AT_FDCWD, target.as_ptr(), times.as_ptr(), 0) };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to restore workspace timestamp {}", path));
    }
    Ok(())
}

pub(super) fn run_workspace_to_host(
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

fn create_workspace_staging_directory(
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

fn remove_workspace_staging_directory(workspace: &WorkspaceMetadata, stage: &str) {
    let _ = run_workspace_utility(workspace, &["rm", "-rf", "--", stage], None);
}

pub(super) fn workspace_path_exists(
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

fn move_workspace_path(
    workspace: &WorkspaceMetadata,
    source: &str,
    target: &str,
    client_stream: Option<&UnixStream>,
) -> Result<()> {
    let output = run_workspace_utility(workspace, &["mv", "--", source, target], client_stream)?;
    ensure_transfer_success("workspace staged move", &output)
}

#[cfg(test)]
pub(super) fn extract_workspace_archive<R: Read>(
    stream: R,
    stage: &HostStagingDirectory,
    source_name: &str,
) -> Result<u64> {
    extract_workspace_archive_with_stats(stream, stage, source_name).map(|(bytes, _)| bytes)
}

pub(super) fn extract_workspace_archive_with_stats<R: Read>(
    stream: R,
    stage: &HostStagingDirectory,
    source_name: &str,
) -> Result<(u64, u64)> {
    let mut archive = Archive::new(stream);
    let mut logical_bytes = 0u64;
    let mut files = 0u64;
    let mut entries = HashSet::new();
    for entry in archive
        .entries()
        .context("failed to read workspace tar entries")?
    {
        let mut entry = entry.context("failed to read workspace tar entry")?;
        let path = entry
            .path()
            .context("workspace tar entry has an invalid path")?
            .into_owned();
        validate_archive_path(&path, source_name)?;
        validate_archive_entry_type(entry.header().entry_type())?;
        if !entries.insert(path.clone()) {
            bail!(
                "workspace tar archive contains duplicate entry '{}'",
                path.display()
            );
        }
        ensure_no_symlink_ancestors(stage.path(), &path)?;
        if entry.header().entry_type().is_file() {
            logical_bytes = logical_bytes.saturating_add(entry.header().size()?);
            files = files.saturating_add(1);
        }
        entry.unpack_in(stage.path()).with_context(|| {
            format!("failed to unpack workspace tar entry '{}'", path.display())
        })?;
    }
    let root = stage.path().join(source_name);
    if fs::symlink_metadata(&root).is_err() {
        bail!("workspace tar archive did not contain expected entry '{source_name}'");
    }
    Ok((logical_bytes, files))
}

pub(super) fn validate_archive_path(path: &Path, source_name: &str) -> Result<()> {
    let mut components = path.components();
    let Some(Component::Normal(root)) = components.next() else {
        bail!(
            "workspace tar archive contains unsafe entry '{}'",
            path.display()
        );
    };
    if root != source_name {
        bail!(
            "workspace tar archive contains unexpected entry '{}'; expected '{}...'",
            path.display(),
            source_name
        );
    }
    for component in components {
        if !matches!(component, Component::Normal(_)) {
            bail!(
                "workspace tar archive contains unsafe entry '{}'",
                path.display()
            );
        }
    }
    Ok(())
}

pub(super) fn validate_archive_entry_type(entry_type: tar::EntryType) -> Result<()> {
    if entry_type.is_file() || entry_type.is_dir() || entry_type.is_symlink() {
        return Ok(());
    }
    bail!("workspace tar archive contains unsupported entry type")
}

fn ensure_no_symlink_ancestors(root: &Path, path: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in path.components() {
        let Component::Normal(component) = component else {
            bail!(
                "workspace tar archive contains unsafe entry '{}'",
                path.display()
            );
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "workspace tar archive writes through symlinked entry '{}'",
                    current.display()
                )
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect staged entry {}", current.display())
                })
            }
        }
    }
    Ok(())
}

pub(super) fn open_host_directory(path: &Path) -> Result<File> {
    if !path.is_absolute() {
        bail!(
            "host destination parent must be absolute: {}",
            path.display()
        );
    }
    let mut current = File::open("/").context("failed to open host root directory")?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                let name = cstring_os(name)?;
                let descriptor = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if descriptor < 0 {
                    return Err(std::io::Error::last_os_error()).with_context(|| {
                        format!(
                            "failed to securely open host destination directory {}",
                            path.display()
                        )
                    });
                }
                current = unsafe { File::from_raw_fd(descriptor) };
            }
            Component::ParentDir | Component::Prefix(_) => {
                bail!(
                    "host destination parent contains unsafe component: {}",
                    path.display()
                )
            }
        }
    }
    Ok(current)
}

fn ensure_host_entry_absent(parent: &File, name: &str) -> Result<()> {
    let name = cstring(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        bail!(
            "host destination '{}' already exists",
            name.to_string_lossy()
        );
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        return Ok(());
    }
    Err(error).context("failed to inspect host destination entry")
}

fn cstring(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| anyhow!("path contains an interior NUL byte"))
}

fn cstring_os(value: &std::ffi::OsStr) -> Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| anyhow!("path contains an interior NUL byte"))
}

pub(super) fn tar_create_args(source: &str, source_name: &str, gzip: bool) -> Vec<String> {
    let parent = Path::new(source)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("/"));
    vec![
        "-C".into(),
        parent.to_string_lossy().into_owned(),
        if gzip { "-czf" } else { "-cf" }.into(),
        "-".into(),
        "--".into(),
        source_name.into(),
    ]
}

pub(super) fn tar_extract_args_at(destination: &str, gzip: bool) -> Vec<String> {
    vec![
        "-C".into(),
        destination.into(),
        if gzip { "-xzpf" } else { "-xpf" }.into(),
        "-".into(),
        "-o".into(),
        "--".into(),
    ]
}

pub(super) fn workspace_tar_command(args: Vec<String>) -> Vec<String> {
    let mut command = vec!["tar".to_string()];
    command.extend(args);
    command
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
    let key = UtilityCacheKey {
        sandbox_id: workspace.sandbox_id.clone(),
        workspace_id: workspace.id.clone(),
        runtime_pid: workspace.runtime_pid.unwrap_or_default(),
        runtime_starttime_ticks: workspace.runtime_starttime_ticks.unwrap_or_default(),
        command: command.first().copied().unwrap_or_default().to_string(),
    };
    if utility_uses_busybox(&key) {
        return run_busybox_utility(workspace, command, client_stream);
    }

    let primary_command = command
        .iter()
        .map(|item| (*item).to_string())
        .collect::<Vec<_>>();
    let primary = spawn_workspace_command(
        workspace,
        "/home",
        &primary_command,
        Stdio::null(),
        Stdio::null(),
        Stdio::piped(),
    )?;
    let mut primary = ChildGuard::new(primary);
    let output = wait_child_output(&mut primary, client_stream)?;
    if output.status.code() != Some(127) {
        cache_utility_choice(key, false);
        return Ok(output);
    }

    cache_utility_choice(key, true);
    run_busybox_utility(workspace, command, client_stream)
}

fn run_busybox_utility(
    workspace: &WorkspaceMetadata,
    command: &[&str],
    client_stream: Option<&UnixStream>,
) -> Result<Output> {
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

fn utility_uses_busybox(key: &UtilityCacheKey) -> bool {
    UTILITY_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|cache| cache.get(key).copied())
        .unwrap_or(false)
}

fn cache_utility_choice(key: UtilityCacheKey, uses_busybox: bool) {
    let Ok(mut cache) = UTILITY_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        return;
    };
    if cache.len() >= UTILITY_CACHE_LIMIT && !cache.contains_key(&key) {
        if let Some(oldest) = cache.keys().next().cloned() {
            cache.remove(&oldest);
        }
    }
    cache.insert(key, uses_busybox);
}

pub(super) fn wait_child_output(
    child: &mut ChildGuard,
    client_stream: Option<&UnixStream>,
) -> Result<Output> {
    let mut stderr_reader = child.child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut stderr = Vec::new();
            pipe.read_to_end(&mut stderr).map(|_| stderr)
        })
    });
    let status = wait_for_child_or_disconnect(child, client_stream, &mut stderr_reader)?;
    let stderr = match stderr_reader {
        Some(reader) => reader
            .join()
            .map_err(|_| anyhow!("child stderr reader panicked"))??,
        None => Vec::new(),
    };
    child.disarm();
    Ok(Output {
        status,
        stdout: Vec::new(),
        stderr,
    })
}

fn wait_for_child_or_disconnect(
    child: &mut ChildGuard,
    client_stream: Option<&UnixStream>,
    stderr_reader: &mut Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
) -> Result<std::process::ExitStatus> {
    if client_stream.is_none() {
        return child
            .child
            .wait()
            .context("failed to wait for child process");
    }

    if let Some(pidfd) = open_pidfd(child.child.id()) {
        let client_fd = client_stream
            .expect("client stream checked above")
            .as_raw_fd();
        let mut descriptors = [
            libc::pollfd {
                fd: pidfd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: client_fd,
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            },
        ];
        loop {
            let poll_result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
            if poll_result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                unsafe { libc::close(pidfd) };
                return Err(error).context("failed waiting for child or client disconnect");
            }
            if descriptors[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                unsafe { libc::close(pidfd) };
                let _ = child.child.kill();
                let _ = child.child.wait();
                if let Some(reader) = stderr_reader.take() {
                    let _ = reader.join();
                }
                bail!("workspace cp client disconnected; transfer cancelled");
            }
            if descriptors[0].revents & libc::POLLIN != 0 {
                unsafe { libc::close(pidfd) };
                return child.child.wait().context("failed to reap child process");
            }
        }
    }

    loop {
        match child
            .child
            .try_wait()
            .context("failed to poll child process")?
        {
            Some(status) => return Ok(status),
            None => {
                if client_stream.is_some_and(|stream| !client_is_connected(stream)) {
                    let _ = child.child.kill();
                    let _ = child.child.wait();
                    if let Some(reader) = stderr_reader.take() {
                        let _ = reader.join();
                    }
                    bail!("workspace cp client disconnected; transfer cancelled")
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn open_pidfd(pid: u32) -> Option<i32> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_uint, 0) as i32 };
    (fd >= 0).then_some(fd)
}

fn set_pipe_capacity(fd: std::os::fd::RawFd) {
    let requested = 1024 * 1024;
    let result = unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, requested) };
    if result < 0 {
        tracing::debug!(fd, error = ?std::io::Error::last_os_error(), "unable to enlarge transfer pipe");
    }
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

#[cfg(test)]
mod tests {
    use super::splice_to_pipe;
    use std::fs;
    use std::io::Read;
    use std::os::fd::FromRawFd;

    #[test]
    fn splice_streams_regular_file_into_pipe_or_reports_unsupported() {
        let path = std::env::temp_dir().join(format!(
            "enclave-splice-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, b"splice fixture").expect("write source");
        let source = fs::File::open(&path).expect("open source");
        let mut pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let result = splice_to_pipe(&source, pipe[1], 14).expect("splice result");
        unsafe { libc::close(pipe[1]) };
        if result {
            let mut output = String::new();
            unsafe { fs::File::from_raw_fd(pipe[0]) }
                .read_to_string(&mut output)
                .expect("read pipe");
            assert_eq!(output, "splice fixture");
        } else {
            unsafe { libc::close(pipe[0]) };
        }
        let _ = fs::remove_file(path);
    }
}
