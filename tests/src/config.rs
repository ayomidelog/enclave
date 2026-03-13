use super::*;
use std::io::Write;

#[test]
fn load_config_returns_defaults_for_nonexistent_implicit_path() {
    let config = load_config(None).unwrap();
    assert!(config.socket.is_none());
    assert!(config.state_dir.is_none());
    assert!(config.wait_secs.is_none());
}

#[test]
fn load_config_fails_for_nonexistent_explicit_path() {
    let result = load_config(Some(Path::new("/nonexistent/config.toml")));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("config file not found"), "got: {}", msg);
}

#[test]
fn load_config_parses_all_fields() {
    let dir = std::env::temp_dir().join(format!("enclave-config-test-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    let mut file = fs::File::create(&path).unwrap();
    writeln!(
        file,
        r#"
socket = "/tmp/enclave.sock"
state_dir = "/var/lib/enclave"
pid_file = "/tmp/enclave.pid"
debootstrap_binary = "/bin/sh"
workspace_apparmor_profile = "enclave-workspace"
workspace_selinux_label = "system_u:system_r:container_t:s0"
suite = "bookworm"
mirror = "http://deb.debian.org/debian"
wait_secs = 15
"#
    )
    .unwrap();

    let config = load_config(Some(&path)).unwrap();
    assert_eq!(
        config.socket.as_deref(),
        Some(Path::new("/tmp/enclave.sock"))
    );
    assert_eq!(
        config.state_dir.as_deref(),
        Some(Path::new("/var/lib/enclave"))
    );
    assert_eq!(
        config.pid_file.as_deref(),
        Some(Path::new("/tmp/enclave.pid"))
    );
    assert_eq!(config.debootstrap_binary.as_deref(), Some("/bin/sh"));
    assert_eq!(
        config.workspace_apparmor_profile.as_deref(),
        Some("enclave-workspace")
    );
    assert_eq!(
        config.workspace_selinux_label.as_deref(),
        Some("system_u:system_r:container_t:s0")
    );
    assert_eq!(config.suite.as_deref(), Some("bookworm"));
    assert_eq!(
        config.mirror.as_deref(),
        Some("http://deb.debian.org/debian")
    );
    assert_eq!(config.wait_secs, Some(15));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_config_parses_partial_config() {
    let dir = std::env::temp_dir().join(format!("enclave-config-partial-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(&path, "wait_secs = 7\n").unwrap();

    let config = load_config(Some(&path)).unwrap();
    assert!(config.socket.is_none());
    assert_eq!(config.wait_secs, Some(7));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_config_rejects_invalid_toml() {
    let dir = std::env::temp_dir().join(format!("enclave-config-invalid-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(&path, "this is not valid toml {{{{").unwrap();

    let result = load_config(Some(&path));
    assert!(result.is_err());

    if dir.exists() {
        fs::remove_dir_all(&dir).expect("failed to cleanup temp test dir");
    }
}

#[test]
fn load_config_rejects_invalid_mirror_url() {
    let dir = std::env::temp_dir().join(format!("enclave-config-badmirror-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(&path, "suite = \"bookworm\"\nmirror = \"https://\"\n").unwrap();

    let result = load_config(Some(&path));
    assert!(result.is_err());

    if dir.exists() {
        fs::remove_dir_all(&dir).expect("failed to cleanup temp test dir");
    }
}

#[test]
fn load_config_rejects_invalid_debootstrap_binary() {
    let dir = std::env::temp_dir().join(format!("enclave-config-badbinary-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(
            &path,
            "debootstrap_binary = \"/path/that/does/not/exist\"\nsuite = \"bookworm\"\nmirror = \"http://deb.debian.org/debian\"\n",
        )
        .unwrap();

    let result = load_config(Some(&path));
    assert!(result.is_err());

    if dir.exists() {
        fs::remove_dir_all(&dir).expect("failed to cleanup temp test dir");
    }
}

#[test]
fn resolve_config_path_prefers_explicit() {
    let path = Path::new("/custom/config.toml");
    assert_eq!(
        resolve_config_path(Some(path)),
        Some(PathBuf::from("/custom/config.toml"))
    );
}
