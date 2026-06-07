use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const AUTH_DIR_NAME: &str = "auth";

const PROVIDERS: [(&str, &str); 3] = [
    ("enclave", "ENCLAVE_TOKEN"),
    ("github", "GITHUB_TOKEN"),
    ("npm", "NPM_TOKEN"),
];

pub fn supported_providers() -> Vec<&'static str> {
    PROVIDERS.iter().map(|(provider, _)| *provider).collect()
}

pub fn provider_env_var(provider: &str) -> Option<&'static str> {
    PROVIDERS
        .iter()
        .find_map(|(name, env)| (*name == provider).then_some(*env))
}

pub fn provider_for_env_var(env_var: &str) -> Option<&'static str> {
    let normalized = env_var.trim().to_ascii_uppercase();
    PROVIDERS
        .iter()
        .find_map(|(name, env)| (*env == normalized).then_some(*name))
}

pub fn validate_provider(provider: &str) -> Result<()> {
    if provider_env_var(provider).is_none() {
        bail!(
            "unsupported auth provider '{}'; supported providers: {}",
            provider,
            supported_providers().join(", ")
        );
    }
    Ok(())
}

pub fn store_token(state_dir: &Path, provider: &str, token: &str) -> Result<PathBuf> {
    validate_provider(provider)?;
    let auth_dir = ensure_auth_dir(state_dir)?;
    let token_path = token_path_for_provider(&auth_dir, provider)?;
    crate::fsutil::write_file_atomic(&token_path, token.as_bytes(), 0o600)
        .with_context(|| format!("failed to write token file {}", token_path.display()))?;
    Ok(token_path)
}

pub fn load_token(state_dir: &Path, provider: &str) -> Result<Option<String>> {
    validate_provider(provider)?;
    let Some(auth_dir) = auth_dir_if_exists(state_dir)? else {
        return Ok(None);
    };
    let token_path = token_path_for_provider(&auth_dir, provider)?;
    if !token_path.exists() {
        return Ok(None);
    }
    validate_token_permissions(&token_path)?;
    let raw = fs::read_to_string(&token_path)
        .with_context(|| format!("failed to read token file {}", token_path.display()))?;
    Ok(Some(raw.trim_end_matches('\n').to_string()))
}

pub fn token_exists(state_dir: &Path, provider: &str) -> Result<bool> {
    validate_provider(provider)?;
    let Some(auth_dir) = auth_dir_if_exists(state_dir)? else {
        return Ok(false);
    };
    let token_path = token_path_for_provider(&auth_dir, provider)?;
    Ok(token_path.exists())
}

pub fn delete_token(state_dir: &Path, provider: &str) -> Result<bool> {
    validate_provider(provider)?;
    let Some(auth_dir) = auth_dir_if_exists(state_dir)? else {
        return Ok(false);
    };
    let token_path = token_path_for_provider(&auth_dir, provider)?;
    if !token_path.exists() {
        return Ok(false);
    }
    fs::remove_file(&token_path)
        .with_context(|| format!("failed to remove token file {}", token_path.display()))?;
    Ok(true)
}

pub fn list_configured_providers(state_dir: &Path) -> Result<Vec<String>> {
    let Some(auth_dir) = auth_dir_if_exists(state_dir)? else {
        return Ok(Vec::new());
    };
    let mut providers = Vec::new();
    for entry in fs::read_dir(&auth_dir)
        .with_context(|| format!("failed to read auth directory {}", auth_dir.display()))?
    {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|file| file.to_str()) else {
            continue;
        };
        let Some(provider) = name.strip_suffix(".token") else {
            continue;
        };
        if provider_env_var(provider).is_some() && validate_token_permissions(&path).is_ok() {
            providers.push(provider.to_string());
        }
    }
    providers.sort();
    providers.dedup();
    Ok(providers)
}

pub fn validate_token_permissions(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat token file {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing to use symlink token file {}", path.display());
    }
    if !metadata.is_file() {
        bail!("token path {} is not a regular file", path.display());
    }
    let expected_uid = unsafe { libc::geteuid() as u32 };
    if metadata.uid() != expected_uid {
        bail!(
            "token file {} must be owned by uid {}, found uid {}",
            path.display(),
            expected_uid,
            metadata.uid()
        );
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        bail!(
            "token file {} must have mode 0600, found {:o}",
            path.display(),
            mode
        );
    }
    Ok(())
}

pub fn auth_dir_if_exists(state_dir: &Path) -> Result<Option<PathBuf>> {
    if !state_dir.exists() {
        return Ok(None);
    }
    let state_dir = fs::canonicalize(state_dir)
        .with_context(|| format!("failed to canonicalize state dir {}", state_dir.display()))?;
    let auth_dir = state_dir.join(AUTH_DIR_NAME);
    if !auth_dir.exists() {
        return Ok(None);
    }
    let auth_dir = crate::fsutil::ensure_path_within(&state_dir, &auth_dir, "auth directory")?;
    Ok(Some(auth_dir))
}

fn ensure_auth_dir(state_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;
    crate::fsutil::ensure_secure_dir(state_dir)?;
    let state_dir = fs::canonicalize(state_dir)
        .with_context(|| format!("failed to canonicalize state dir {}", state_dir.display()))?;

    let auth_dir = state_dir.join(AUTH_DIR_NAME);
    fs::create_dir_all(&auth_dir)
        .with_context(|| format!("failed to create auth dir {}", auth_dir.display()))?;
    crate::fsutil::ensure_secure_dir(&auth_dir)?;
    crate::fsutil::ensure_path_within(&state_dir, &auth_dir, "auth directory")
}

pub fn token_path_for_provider(auth_dir: &Path, provider: &str) -> Result<PathBuf> {
    validate_provider(provider)?;
    let file = format!("{provider}.token");
    Ok(auth_dir.join(file))
}

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
