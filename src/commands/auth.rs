use std::io::Write;

use anyhow::{bail, Context, Result};

use crate::auth::AuthManager;
use crate::cli::{AuthCommands, AuthProviderArgs};
use crate::paths;

pub(crate) fn run_auth_command(command: AuthCommands) -> Result<()> {
    let manager = AuthManager::new(paths::default_state_dir());
    match command {
        AuthCommands::Login(args) => run_auth_login(&manager, args),
        AuthCommands::List => run_auth_list(&manager),
        AuthCommands::Logout(args) => run_auth_logout(&manager, args),
    }
}

fn run_auth_login(manager: &AuthManager, args: AuthProviderArgs) -> Result<()> {
    if manager.token_exists(&args.provider)? && !confirm_overwrite(&args.provider)? {
        println!("aborted");
        return Ok(());
    }

    eprint!("Enter token for provider \"{}\": ", args.provider);
    std::io::stderr().flush()?;
    let token = read_hidden_token_line()?;
    if token.trim().is_empty() {
        bail!("token must not be empty");
    }
    manager.store_token(&args.provider, token.trim())?;
    println!("stored token for provider \"{}\"", args.provider);
    Ok(())
}

fn run_auth_list(manager: &AuthManager) -> Result<()> {
    let configured = manager.list_providers()?;
    print!("{}", format_auth_provider_list(&configured));
    Ok(())
}

fn format_auth_provider_list(configured: &[String]) -> String {
    let mut output = String::from("Supported auth providers:\n");
    for provider in crate::auth::supported_providers() {
        let configured_suffix = if configured.iter().any(|item| item == provider) {
            " (configured)"
        } else {
            ""
        };
        output.push_str(&format!("- {}{}\n", provider, configured_suffix));
    }
    output
}

fn run_auth_logout(manager: &AuthManager, args: AuthProviderArgs) -> Result<()> {
    if manager.delete_token(&args.provider)? {
        println!("removed token for provider \"{}\"", args.provider);
    } else {
        println!("no token configured for provider \"{}\"", args.provider);
    }
    Ok(())
}

fn confirm_overwrite(provider: &str) -> Result<bool> {
    eprint!(
        "Token for provider \"{}\" already exists. Overwrite? [y/N]: ",
        provider
    );
    std::io::stderr().flush()?;
    let mut input = String::new();
    let read = std::io::stdin()
        .read_line(&mut input)
        .context("failed to read confirmation input")?;
    if read == 0 {
        return Ok(false);
    }
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn read_hidden_token_line() -> Result<String> {
    let stdin_fd = libc::STDIN_FILENO;
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    let is_tty = unsafe { libc::isatty(stdin_fd) == 1 };
    let mut disabled_echo = false;

    if is_tty {
        let tcgetattr_rc = unsafe { libc::tcgetattr(stdin_fd, &mut term) };
        if tcgetattr_rc == 0 {
            let mut no_echo = term;
            no_echo.c_lflag &= !libc::ECHO;
            let tcsetattr_rc = unsafe { libc::tcsetattr(stdin_fd, libc::TCSANOW, &no_echo) };
            if tcsetattr_rc == 0 {
                disabled_echo = true;
            }
        }
    }

    let mut line = String::new();
    let read_result = std::io::stdin().read_line(&mut line);

    if disabled_echo {
        let _ = unsafe { libc::tcsetattr(stdin_fd, libc::TCSANOW, &term) };
        eprintln!();
    }

    let read = read_result.context("failed to read token input")?;
    if read == 0 {
        bail!("no token provided on stdin");
    }
    Ok(line.trim_end_matches('\n').to_string())
}

#[cfg(test)]
#[path = "../../tests/src/commands/auth.rs"]
mod tests;
