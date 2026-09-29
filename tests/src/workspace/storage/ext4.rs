use std::fs;

use super::{filesystem_size, parse_geometry, parse_superblock};
use crate::workspace::storage::{resize_workspace_disk_allocation, workspace_disk_image_path};
use crate::workspace::types::{WorkspaceLimits, WorkspaceMetadata};
use crate::workspace::WorkspaceStatus;

fn superblock_bytes(block_size_shift: u32, blocks: u64, bits64: bool) -> [u8; 1024] {
    let mut superblock = [0u8; 1024];
    superblock[0x38..0x3a].copy_from_slice(&0xEF53u16.to_le_bytes());
    superblock[0x04..0x08].copy_from_slice(&(blocks as u32).to_le_bytes());
    superblock[0x18..0x1c].copy_from_slice(&block_size_shift.to_le_bytes());
    if bits64 {
        superblock[0x60..0x64].copy_from_slice(&0x80u32.to_le_bytes());
        superblock[0x150..0x154].copy_from_slice(&((blocks >> 32) as u32).to_le_bytes());
    }
    superblock
}

#[test]
fn parse_superblock_reads_a_4k_filesystem_size() {
    // 4 KiB blocks, 512 blocks -> 2 MiB.
    let superblock = superblock_bytes(2, 512, false);
    assert_eq!(parse_superblock(&superblock).unwrap(), 2 * 1024 * 1024);
}

#[test]
fn parse_superblock_reads_the_high_block_count_for_64_bit_filesystems() {
    let blocks = (5u64 << 32) | 7;
    let superblock = superblock_bytes(0, blocks, true);
    assert_eq!(parse_superblock(&superblock).unwrap(), blocks * 1024);
}

#[test]
fn parse_superblock_rejects_foreign_filesystems() {
    let superblock = [0u8; 1024];
    let error = parse_superblock(&superblock).expect_err("zeroed superblock must be rejected");
    assert!(error.to_string().contains("not an ext2/3/4 filesystem"));
}

#[test]
fn parse_superblock_rejects_an_impossible_block_size() {
    let superblock = superblock_bytes(9, 8, false);
    let error = parse_superblock(&superblock).expect_err("block shift 9 must be rejected");
    assert!(error
        .to_string()
        .contains("unsupported ext4 block size shift"));
}

/// The block count is what a resize is expressed in, so it has to be exact.
///
/// A resize that asked `resize2fs` for a size rounded the wrong way would leave the
/// filesystem and the image file a fraction of a block apart, which is the state the
/// whole module exists to avoid.
#[test]
fn geometry_reports_the_block_size_and_count_a_resize_needs() {
    let superblock = superblock_bytes(2, 512, false);
    let geometry = parse_geometry(&superblock).unwrap();
    assert_eq!(geometry.block_size, 4096);
    assert_eq!(geometry.block_count, 512);
    assert_eq!(geometry.size_bytes(), 2 * 1024 * 1024);
}

#[test]
fn geometry_rounds_a_byte_count_down_to_whole_blocks() {
    let superblock = superblock_bytes(2, 512, false);
    let geometry = parse_geometry(&superblock).unwrap();
    // Exactly one block.
    assert_eq!(geometry.blocks_in(4096), 1);
    // One byte short of a block is no blocks at all, rather than a block that would
    // overrun the size that was asked for.
    assert_eq!(geometry.blocks_in(4095), 0);
    assert_eq!(geometry.blocks_in(8192), 2);
    assert_eq!(geometry.blocks_in(8191), 1);
}

fn resize_fixture(temp_dir: &std::path::Path) -> WorkspaceMetadata {
    let workspace_path = temp_dir.join("workspace");
    WorkspaceMetadata {
        id: "workspace-id".to_string(),
        sandbox_id: "sandbox-id".to_string(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        workspace_path: workspace_path.to_string_lossy().to_string(),
        filesystem_path: workspace_path.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: temp_dir.join("rootfs").to_string_lossy().to_string(),
        overlay_home_base_path: temp_dir.join("home-base").to_string_lossy().to_string(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        owner: None,
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: WorkspaceStatus::Stopped,
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits {
            disk_bytes: Some(32 * 1024 * 1024),
            ..WorkspaceLimits::default()
        },
        assigned_ip: None,
    }
}

fn tool_is_available(tool: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn filesystem_size_reports_a_readable_error_for_a_foreign_image() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-ext4-size-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    let image = temp_dir.join("fs.img");
    fs::write(&image, vec![0u8; 4096]).unwrap();

    let error = filesystem_size(&image).expect_err("a zeroed image is not an ext4 filesystem");
    assert!(
        format!("{error:#}").contains("not an ext2/3/4 filesystem"),
        "unexpected error: {error:#}"
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn failed_resize_restores_the_previous_image_size() {
    if !tool_is_available("e2fsck") || !tool_is_available("truncate") {
        return;
    }
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-resize-rollback-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(temp_dir.join("workspace")).unwrap();
    let workspace = resize_fixture(&temp_dir);
    // A zeroed file is not an ext4 filesystem, so the check after the image has
    // been grown fails and the rollback must restore the previous size.
    let image = workspace_disk_image_path(&workspace);
    let file = fs::File::create(&image).unwrap();
    file.set_len(32 * 1024 * 1024).unwrap();
    drop(file);

    let error = resize_workspace_disk_allocation(&workspace, 64 * 1024 * 1024)
        .expect_err("a resize that cannot check the filesystem must fail");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("restored workspace disk image"),
        "expected the image to be restored, got: {rendered}"
    );
    assert_eq!(fs::metadata(&image).unwrap().len(), 32 * 1024 * 1024);

    let _ = fs::remove_dir_all(&temp_dir);
}
