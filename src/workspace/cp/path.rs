use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
    HostToWorkspace,
    WorkspaceToHost,
}

impl Direction {
    pub(super) fn parse(raw: &str) -> Result<Self> {
        match raw {
            "host_to_workspace" => Ok(Self::HostToWorkspace),
            "workspace_to_host" => Ok(Self::WorkspaceToHost),
            _ => bail!(
                "invalid workspace cp direction '{}'; expected host_to_workspace or workspace_to_host",
                raw
            ),
        }
    }
}

#[derive(Debug)]
pub(super) struct DestinationPlan {
    pub(super) parent: PathBuf,
    pub(super) rename_to: Option<String>,
}

pub(super) fn validate_direction_paths(src: &str, dst: &str, direction: Direction) -> Result<()> {
    match direction {
        Direction::HostToWorkspace => {
            validate_host_path(src, "source")?;
            validate_workspace_path(dst, "destination")?;
        }
        Direction::WorkspaceToHost => {
            validate_workspace_path(src, "source")?;
            validate_host_path(dst, "destination")?;
        }
    }
    Ok(())
}

pub(super) fn validate_host_source(path: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("host source '{}' does not exist", path))?;
    if metadata.file_type().is_block_device() || metadata.file_type().is_char_device() {
        bail!("refusing to copy device node '{}'", path);
    }
    Ok(())
}

pub(super) fn validate_host_destination(path: &str) -> Result<()> {
    let mut current = PathBuf::new();
    for component in Path::new(path).components() {
        current.push(component.as_os_str());
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing to copy through symlinked host destination component '{}'",
                current.display()
            );
        }
        if metadata.file_type().is_block_device() || metadata.file_type().is_char_device() {
            bail!("refusing to overwrite host device node '{}'", path);
        }
    }
    Ok(())
}

fn validate_host_path(path: &str, label: &str) -> Result<()> {
    if path.is_empty() {
        bail!("{} path cannot be empty", label);
    }
    if path == "/" {
        bail!("refusing to copy host root");
    }
    Ok(())
}

fn validate_workspace_path(path: &str, label: &str) -> Result<()> {
    let path = Path::new(path);
    if !path.is_absolute() {
        bail!("workspace {} path must be absolute", label);
    }
    if path == Path::new("/") {
        bail!("refusing to copy workspace root");
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!(
            "workspace {} path cannot contain '..': {}",
            label,
            path.display()
        );
    }
    Ok(())
}

pub(super) fn source_name(path: &str) -> Result<String> {
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("source path '{}' has no filename", path))?;
    if matches!(name.as_str(), "." | "..") {
        bail!("source path '{}' has an unsafe filename", path);
    }
    Ok(name)
}

pub(super) fn destination_plan(
    destination: &str,
    source_name: &str,
    destination_is_dir: bool,
) -> Result<DestinationPlan> {
    if destination_is_dir {
        return Ok(DestinationPlan {
            parent: PathBuf::from(destination),
            rename_to: None,
        });
    }

    let destination_path = Path::new(destination);
    let parent = destination_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("/"));
    let destination_name = destination_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("destination '{}' has no filename", destination))?;
    if matches!(destination_name, "." | "..") {
        bail!("destination '{}' has an unsafe filename", destination);
    }
    Ok(DestinationPlan {
        parent: parent.to_path_buf(),
        rename_to: (source_name != destination_name).then(|| destination_name.to_string()),
    })
}

impl DestinationPlan {
    pub(super) fn extracted_path(&self, source_name: &str) -> PathBuf {
        self.parent.join(source_name)
    }

    pub(super) fn final_path(&self, source_name: &str) -> PathBuf {
        self.parent
            .join(self.rename_to.as_deref().unwrap_or(source_name))
    }
}
