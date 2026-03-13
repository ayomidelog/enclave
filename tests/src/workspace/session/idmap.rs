use super::*;
use crate::workspace::session::userns::{IdMapRange, UserNamespacePlan};

#[test]
fn build_idmap_entries_maps_root_to_host_owner_and_rest_to_identity() {
    let entries = build_idmap_entries('u', 100_000, 65_536, 1_000);
    assert_eq!(
        entries,
        vec![
            IdMapEntry {
                id_type: 'u',
                mount_start: 100_000,
                host_start: 1_000,
                count: 1,
            },
            IdMapEntry {
                id_type: 'u',
                mount_start: 100_001,
                host_start: 100_001,
                count: 65_535,
            },
        ]
    );
}

#[test]
fn build_idmap_entries_splits_identity_range_when_owner_overlaps() {
    let entries = build_idmap_entries('g', 100_000, 10, 100_004);
    assert_eq!(
        entries,
        vec![
            IdMapEntry {
                id_type: 'g',
                mount_start: 100_000,
                host_start: 100_004,
                count: 1,
            },
            IdMapEntry {
                id_type: 'g',
                mount_start: 100_001,
                host_start: 100_001,
                count: 3,
            },
            IdMapEntry {
                id_type: 'g',
                mount_start: 100_005,
                host_start: 100_005,
                count: 5,
            },
        ]
    );
}

#[test]
fn workspace_bind_mount_idmap_option_renders_both_uid_and_gid_maps() {
    let dir = std::env::temp_dir().join(format!(
        "enclave-idmap-option-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");

    let plan = UserNamespacePlan {
        owner: "tester".to_string(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: 100_000,
            count: 65_536,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: 100_000,
            count: 65_536,
        },
    };

    let rendered = workspace_bind_mount_idmap_option(&dir, &plan)
        .expect("render idmap")
        .expect("non-identity idmap should be present");
    assert!(rendered.contains("u:100000:"));
    assert!(rendered.contains("g:100000:"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn workspace_bind_mount_idmap_option_skips_identity_map() {
    let dir = std::env::temp_dir().join(format!(
        "enclave-idmap-identity-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");

    let plan = UserNamespacePlan {
        owner: "root".to_string(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: fs::metadata(&dir).expect("stat dir").uid(),
            count: 1,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: fs::metadata(&dir).expect("stat dir").gid(),
            count: 1,
        },
    };

    let rendered = workspace_bind_mount_idmap_option(&dir, &plan).expect("render idmap");
    assert!(rendered.is_none());

    let _ = fs::remove_dir_all(&dir);
}
