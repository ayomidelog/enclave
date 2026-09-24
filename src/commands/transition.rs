//! Reporting the state change a lifecycle response describes.
//!
//! Every lifecycle response carries the state the target was in and the state it
//! finished in, so the command can say what changed rather than only what the
//! target is now. That distinction is what a caller needs to tell a stop that
//! worked from a stop that was already stopped, and it is the same pair the daemon
//! reports in its JSON.
//!
//! Only a real change is printed. An operation that found the target in its target
//! state did nothing, and printing "stopped -> stopped" would suggest otherwise.

use serde_json::Value;

/// The state pair a lifecycle response describes, when it carries one.
pub(crate) fn transition_of(response: &Value) -> Option<(&str, &str)> {
    let previous = response.get("previous_state").and_then(Value::as_str)?;
    let current = response.get("current_state").and_then(Value::as_str)?;
    Some((previous, current))
}

/// Print the state change a lifecycle response describes, when there is one.
pub(crate) fn print_state_transition(response: &Value) {
    let Some((previous, current)) = transition_of(response) else {
        return;
    };
    if previous == current {
        return;
    }
    println!("state: {previous} -> {current}");
}

#[cfg(test)]
#[path = "../../tests/src/commands/transition.rs"]
mod tests;
