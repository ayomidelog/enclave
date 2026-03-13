use std::env;
use std::path::PathBuf;

const APP_NAME: &str = "enclave";

pub fn default_runtime_dir() -> PathBuf {
    let uid = current_uid();
    if uid == 0 {
        return PathBuf::from("/run").join(APP_NAME);
    }

    if let Ok(xdg_runtime_dir) = env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(xdg_runtime_dir).join(APP_NAME);
    }

    PathBuf::from(format!("/run/user/{}/{}", uid, APP_NAME))
}

pub fn default_state_dir() -> PathBuf {
    let uid = current_uid();
    if uid == 0 {
        return PathBuf::from("/root/.local/state").join(APP_NAME);
    }

    if let Ok(xdg_state_home) = env::var("XDG_STATE_HOME") {
        return PathBuf::from(xdg_state_home).join(APP_NAME);
    }

    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join(APP_NAME);
    }

    default_runtime_dir().join("state")
}

pub fn default_socket_path() -> PathBuf {
    default_runtime_dir().join("manager.sock")
}

pub fn default_pid_file() -> PathBuf {
    default_runtime_dir().join("manager.pid")
}

fn current_uid() -> u32 {
    unsafe { libc::geteuid() as u32 }
}
