//! Reading a token value from standard input.
//!
//! This is the only place a token value is read, and it never comes from an
//! argument or the environment: an argument is visible in the process list to
//! every user on the host, and so is the environment of a running process.

use std::io::Write;

use anyhow::{bail, Context, Result};

/// Read one line, without echoing it when the input is a terminal.
///
/// Echo is turned off for an interactive terminal so the value does not appear
/// on screen, and left alone otherwise so a pipe is read exactly as written.
/// The terminal state is restored before returning, including on the failure
/// path, because leaving a terminal with echo off is worse than the failure.
pub(super) fn read_token_line() -> Result<String> {
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

/// Prompt on stderr, so a token read from a pipe is not preceded by a prompt.
pub(super) fn prompt_for(provider: &str, user: Option<&str>) {
    match user {
        Some(user) => eprint!("Enter token for provider \"{provider}\" (user \"{user}\"): "),
        None => eprint!("Enter token for provider \"{provider}\": "),
    }
    let _ = std::io::stderr().flush();
}
