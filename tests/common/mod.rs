use std::path::PathBuf;

pub fn enclave_bin() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_enclave") {
        return PathBuf::from(path);
    }

    let exe = std::env::current_exe().expect("failed to resolve current test executable path");
    let debug_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("failed to resolve target debug directory");
    debug_dir.join("enclave")
}
