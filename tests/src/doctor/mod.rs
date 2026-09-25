//! Tests for the doctor.
//!
//! A check that panics is worse than one that reports nothing, because it takes the
//! whole report with it, so several of these only assert that a check survives the
//! state they were given.

use super::*;

mod checks;
mod mounts;
mod repair;
mod report;
