use super::output_to_string;
use super::transport::{ClientDisconnected, HelperClosed, HelperTimeout};

#[test]
fn persistent_output_decodes_lossy_utf8() {
    assert_eq!(output_to_string(vec![b'o', b'k']), "ok");
}

#[test]
fn a_closed_helper_is_a_typed_recoverable_failure() {
    let error = anyhow::Error::new(HelperClosed {
        message: "helper closed".to_string(),
    });
    assert!(HelperClosed::is_in(&error));
    assert!(!HelperTimeout::is_in(&error));
}

#[test]
fn a_helper_timeout_is_not_mistaken_for_a_closed_connection() {
    let error = anyhow::Error::new(HelperTimeout {
        message: "helper timed out".to_string(),
    });
    assert!(HelperTimeout::is_in(&error));
    assert!(!HelperClosed::is_in(&error));
}

#[test]
fn a_client_disconnect_is_a_typed_failure() {
    let error = anyhow::Error::new(ClientDisconnected {
        message: "client disconnected".to_string(),
    });
    assert!(ClientDisconnected::is_in(&error));
}

#[test]
fn each_helper_start_gets_a_unique_socket() {
    let first = super::helper::helper_socket_path(41, 9001);
    let second = super::helper::helper_socket_path(41, 9001);
    assert_ne!(first, second);
    assert_eq!(first.parent(), Some(std::path::Path::new("/run/enclave")));
    assert!(first
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.starts_with("session-41-9001-")));
    let fixed = super::helper::helper_socket_path_for_test(
        41,
        9001,
        uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap(),
    );
    assert_eq!(
        fixed,
        std::path::PathBuf::from(
            "/run/enclave/session-41-9001-00000000-0000-4000-8000-000000000001.sock"
        )
    );
}
