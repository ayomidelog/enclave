//! Finding the loop device that backs an image.

use super::*;

#[test]
fn sysfs_loop_scan_reads_attached_backings_and_skips_free_devices() {
    let sys_block = loopback_fixture_dir("scan");
    write_backing(&sys_block, "loop0", "/tmp/a/fs.img\n");
    // An unattached device has the directory but no backing file.
    fs::create_dir_all(sys_block.join("loop1/loop")).unwrap();
    write_backing(&sys_block, "loop2", "/tmp/b/fs.img");
    // A non-loop block device is not a loop device.
    write_backing(&sys_block, "sda", "/tmp/ignored");

    let devices = sysfs_loop_devices_in(&sys_block).unwrap();
    assert_eq!(
        devices,
        vec![
            LoopDevice {
                device: "/dev/loop0".to_string(),
                backing: PathBuf::from("/tmp/a/fs.img"),
            },
            LoopDevice {
                device: "/dev/loop2".to_string(),
                backing: PathBuf::from("/tmp/b/fs.img"),
            },
        ]
    );
    let _ = fs::remove_dir_all(&sys_block);
}

#[test]
fn sysfs_loop_scan_orders_by_device_index_and_reports_missing_sysfs() {
    let sys_block = loopback_fixture_dir("order");
    write_backing(&sys_block, "loop10", "/tmp/ten");
    write_backing(&sys_block, "loop2", "/tmp/two");

    let devices = sysfs_loop_devices_in(&sys_block).unwrap();
    assert_eq!(
        devices
            .iter()
            .map(|d| d.device.as_str())
            .collect::<Vec<_>>(),
        vec!["/dev/loop2", "/dev/loop10"]
    );
    assert!(sysfs_loop_devices_in(&sys_block.join("absent")).is_none());
    let _ = fs::remove_dir_all(&sys_block);
}

#[test]
fn loop_device_selection_matches_the_recorded_backing() {
    let devices = vec![
        LoopDevice {
            device: "/dev/loop0".to_string(),
            backing: PathBuf::from("/tmp/one/fs.img"),
        },
        LoopDevice {
            device: "/dev/loop1".to_string(),
            backing: PathBuf::from("/tmp/two/fs.img"),
        },
    ];

    assert_eq!(
        select_backing(&devices, std::path::Path::new("/tmp/two/fs.img")),
        vec!["/dev/loop1"]
    );
    assert!(select_backing(&devices, std::path::Path::new("/tmp/absent/fs.img")).is_empty());
}
