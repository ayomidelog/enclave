//! Writing resolved tokens into a workspace, and the wrapper that reads them.
//!
//! Tokens are never placed in a workspace's environment by the daemon. They are
//! written as files inside the workspace's own mount namespace, and a small
//! wrapper script reads them when it runs a command, so a token is only in the
//! environment of the process that asked for it.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

use super::providers::{supported_providers, PROVIDERS};
use super::storage;
use super::WorkspaceAuthToken;

/// Write the resolved credentials into the workspace's namespace.
///
/// `workspace_rootfs` is the workspace's `/proc/<pid>/root`, not the shared
/// sandbox rootfs: writing there would put one workspace's credentials in a
/// directory every other workspace on the sandbox can read.
///
/// `tokens` are written to the auth directory, where the wrapper's provider loop
/// reads them, and `env_tokens` to the env directory, where the wrapper exports
/// whatever it finds under the file's own name.
pub(super) fn write_workspace_auth(
    workspace_rootfs: &str,
    tokens: &[WorkspaceAuthToken],
    env_tokens: &[WorkspaceAuthToken],
) -> Result<()> {
    let rootfs = PathBuf::from(workspace_rootfs);
    if !rootfs.is_absolute() {
        anyhow::bail!(
            "workspace rootfs path must be absolute: {}",
            rootfs.display()
        );
    }
    if !is_workspace_namespace_root(&rootfs) {
        anyhow::bail!(
            "workspace rootfs path must be /proc/<pid>/root: {}",
            rootfs.display()
        );
    }

    // The destination is inside the workspace's own mount namespace, where `/run`
    // is a tmpfs the session mounted for this start, so the files are always
    // absent when this runs and there is never an unchanged copy to compare
    // against. Rewriting them unconditionally is therefore not the waste it
    // looks like: measured on this host these writes are about 210 us with every
    // provider and env token configured, which is under 0.2% of a start. Skipping
    // an unchanged write was tried and removed again, because it saves nothing
    // here and the reconcile it needs is more code than the remove-then-write it
    // replaced. The audit append the caller does afterwards is a separate cost,
    // and it is the only part of the phase that grows with the token count.
    //
    // The writes are best-effort rather than durable. A durable write fsyncs,
    // and there is nothing here for an fsync to protect: the tmpfs is inside a
    // mount namespace that dies with the runtime, so the files cannot outlive
    // the process that reads them, and a power loss takes the runtime and the
    // namespace with it. A write without fsync is already visible to every
    // process in the namespace, which is the only reader there is.
    let auth_dir = rootfs.join("run/enclave/auth");
    fs::create_dir_all(&auth_dir)
        .with_context(|| format!("failed to create {}", auth_dir.display()))?;
    let env_dir = rootfs.join("run/enclave/env");
    fs::create_dir_all(&env_dir)
        .with_context(|| format!("failed to create {}", env_dir.display()))?;

    for provider in supported_providers() {
        let token_path = storage::token_path_for_name(&auth_dir, provider)?;
        if token_path.exists() {
            fs::remove_file(&token_path)
                .with_context(|| format!("failed to remove {}", token_path.display()))?;
        }
    }
    for entry in
        fs::read_dir(&env_dir).with_context(|| format!("failed to read {}", env_dir.display()))?
    {
        let path = entry?.path();
        if path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }

    for token in tokens {
        let token_path = storage::token_path_for_name(&auth_dir, &token.name)?;
        crate::fsutil::write_file_atomic_with(
            &token_path,
            token.token.as_bytes(),
            0o400,
            crate::fsutil::Durability::BestEffort,
        )
        .with_context(|| format!("failed to write {}", token_path.display()))?;
    }
    for token in env_tokens {
        let token_path = env_dir.join(&token.env_var);
        crate::fsutil::write_file_atomic_with(
            &token_path,
            token.token.as_bytes(),
            0o400,
            crate::fsutil::Durability::BestEffort,
        )
        .with_context(|| format!("failed to write {}", token_path.display()))?;
    }
    Ok(())
}

/// The script a workspace runs its commands through, which exports the tokens.
///
/// It is generated from the provider table rather than written by hand so that
/// adding a provider cannot leave the wrapper behind: the two are the same list.
pub fn workspace_env_wrapper_script() -> String {
    let mut script = String::from("for provider in");
    for (provider, _) in PROVIDERS {
        script.push(' ');
        script.push_str(provider);
    }
    script.push_str(
        "; do\n  token_file=\"/run/enclave/auth/${provider}.token\"\n  if [ -r \"$token_file\" ]; then\n    token=\"$(cat \"$token_file\")\"\n    case \"$provider\" in\n",
    );
    for (provider, env_var) in PROVIDERS {
        if provider == "github" {
            script.push_str(&format!(
                "      {provider}) export {env_var}=\"$token\"; export GH_TOKEN=\"$token\" ;;\n"
            ));
        } else {
            script.push_str(&format!(
                "      {provider}) export {env_var}=\"$token\" ;;\n"
            ));
        }
    }
    script.push_str(
        r#"    esac
  fi
done
if [ -n "$GITHUB_TOKEN" ]; then
  _cfg_n="${GIT_CONFIG_COUNT:-0}"
  export "GIT_CONFIG_KEY_${_cfg_n}=credential.helper"
  export "GIT_CONFIG_VALUE_${_cfg_n}=!f(){ echo username=x-access-token; echo \"password=\$GITHUB_TOKEN\"; }; f"
  _cfg_n=$((_cfg_n + 1))
  export GIT_CONFIG_COUNT="$_cfg_n"
  export GIT_TERMINAL_PROMPT=0
fi
for token_file in /run/enclave/env/*; do
  if [ ! -r "$token_file" ]; then
    continue
  fi
  env_name="${token_file##*/}"
  case "$env_name" in
    ""|[0-9]*|*[!A-Z0-9_]*)
      continue
      ;;
  esac
  token="$(cat "$token_file")"
  export "${env_name}=${token}"
done
cd "$1" && shift && exec "$@""#,
    );
    script
}

fn is_workspace_namespace_root(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::RootDir))
        && matches!(
            components.next(),
            Some(Component::Normal(part)) if part == "proc"
        )
        && matches!(
            components.next(),
            Some(Component::Normal(part)) if is_valid_pid_component(part)
        )
        && matches!(
            components.next(),
            Some(Component::Normal(part)) if part == "root"
        )
        && components.next().is_none()
}

fn is_valid_pid_component(part: &std::ffi::OsStr) -> bool {
    part.to_str()
        .is_some_and(|value| !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()))
}
