use super::*;
use std::sync::{Mutex, OnceLock};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn choose_owner_prefers_override_then_sudo_user() {
    let _guard = env_lock()
        .lock()
        .expect("failed to acquire environment variable lock");
    let uid = vec![
        SubordinateIdRange {
            owner: "alice".to_string(),
            start: 100_000,
            count: 65_536,
        },
        SubordinateIdRange {
            owner: "bob".to_string(),
            start: 165_536,
            count: 65_536,
        },
    ];
    let gid = uid.clone();

    unsafe {
        env::set_var(OWNER_OVERRIDE_ENV, "bob");
        env::set_var("SUDO_USER", "alice");
    }
    let chosen = choose_owner(&uid, &gid).expect("choose owner");
    assert_eq!(chosen, "bob");
    unsafe {
        env::remove_var(OWNER_OVERRIDE_ENV);
        env::remove_var("SUDO_USER");
    }
}

#[test]
fn preferred_owners_uses_effective_user_before_sudo_user() {
    let _guard = env_lock()
        .lock()
        .expect("failed to acquire environment variable lock");
    unsafe {
        env::remove_var(OWNER_OVERRIDE_ENV);
        env::set_var("USER", "root");
        env::set_var("LOGNAME", "root");
        env::set_var("SUDO_USER", "alice");
    }
    let chosen = preferred_owners_with_effective_user(Some("root"));
    assert_eq!(chosen, vec!["root".to_string(), "alice".to_string()]);
    unsafe {
        env::remove_var("USER");
        env::remove_var("LOGNAME");
        env::remove_var("SUDO_USER");
    }
}

#[test]
fn preferred_owners_keeps_sudo_user_after_current_login_fallbacks() {
    let _guard = env_lock()
        .lock()
        .expect("failed to acquire environment variable lock");
    unsafe {
        env::remove_var(OWNER_OVERRIDE_ENV);
        env::set_var("USER", "service");
        env::set_var("LOGNAME", "operator");
        env::set_var("SUDO_USER", "alice");
    }
    let chosen = preferred_owners_with_effective_user(None);
    assert_eq!(
        chosen,
        vec![
            "service".to_string(),
            "operator".to_string(),
            "alice".to_string()
        ]
    );
    unsafe {
        env::remove_var("USER");
        env::remove_var("LOGNAME");
        env::remove_var("SUDO_USER");
    }
}

#[test]
fn choose_owner_accepts_single_common_owner() {
    let uid = vec![SubordinateIdRange {
        owner: "carol".to_string(),
        start: 100_000,
        count: 65_536,
    }];
    let gid = uid.clone();
    let chosen = choose_owner(&uid, &gid).expect("single owner");
    assert_eq!(chosen, "carol");
}

#[test]
fn identity_user_namespace_plan_maps_single_uid_and_gid() {
    let plan = identity_user_namespace_plan("root", 0, 0);
    assert_eq!(
        plan,
        UserNamespacePlan {
            owner: "root".to_string(),
            uid_map: IdMapRange {
                inner_start: 0,
                outer_start: 0,
                count: 1,
            },
            gid_map: IdMapRange {
                inner_start: 0,
                outer_start: 0,
                count: 1,
            },
        }
    );
}

#[test]
fn subordinate_user_namespace_plan_uses_preferred_owner_range() {
    let _guard = env_lock()
        .lock()
        .expect("failed to acquire environment variable lock");
    unsafe {
        env::remove_var(OWNER_OVERRIDE_ENV);
        env::set_var("SUDO_USER", "alice");
    }
    let dir = std::env::temp_dir().join(format!(
        "enclave-userns-subid-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    let subuid = dir.join("subuid");
    let subgid = dir.join("subgid");
    fs::write(&subuid, "alice:100000:65536\n").expect("write subuid");
    fs::write(&subgid, "alice:100000:65536\n").expect("write subgid");

    let uid_ranges = parse_subordinate_id_file(subuid.to_string_lossy().as_ref(), "subuid")
        .expect("parse subuid");
    let gid_ranges = parse_subordinate_id_file(subgid.to_string_lossy().as_ref(), "subgid")
        .expect("parse subgid");
    let owner = choose_owner(&uid_ranges, &gid_ranges).expect("choose owner");
    assert_eq!(owner, "alice");
    let plan = subordinate_user_namespace_plan_from_ranges(&uid_ranges, &gid_ranges)
        .expect("build subordinate plan");
    assert_eq!(plan.owner, "alice");
    assert_eq!(plan.uid_map.outer_start, 100_000);
    assert_eq!(plan.uid_map.count, 65_536);
    assert_eq!(plan.gid_map.outer_start, 100_000);
    assert_eq!(plan.gid_map.count, 65_536);

    let _ = fs::remove_dir_all(&dir);
    unsafe {
        env::remove_var("SUDO_USER");
    }
}

#[test]
fn root_can_use_subordinate_plan_only_for_root_owned_ranges() {
    let root_plan = UserNamespacePlan {
        owner: "root".to_string(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: 200_000,
            count: 65_536,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: 200_000,
            count: 65_536,
        },
    };
    let user_plan = UserNamespacePlan {
        owner: "alice".to_string(),
        ..root_plan.clone()
    };
    assert!(root_can_use_subordinate_plan(&root_plan));
    assert!(!root_can_use_subordinate_plan(&user_plan));
}

#[test]
fn owner_name_or_uid_falls_back_to_uid_label() {
    assert_eq!(owner_name_or_uid(None, 0), "uid-0");
    assert_eq!(
        owner_name_or_uid(Some("root".to_string()), 0),
        "root".to_string()
    );
}

#[test]
fn parse_subordinate_id_file_rejects_invalid_line() {
    let dir = std::env::temp_dir().join(format!(
        "enclave-userns-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    let file = dir.join("subuid");
    fs::write(&file, "broken-line\n").expect("write invalid data");
    let err = parse_subordinate_id_file(file.to_string_lossy().as_ref(), "subuid")
        .expect_err("invalid file should fail");
    assert!(err.to_string().contains("invalid subuid line"));
    let _ = fs::remove_dir_all(&dir);
}
