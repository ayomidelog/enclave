//! The operation journal: one durable record per lifecycle operation.
//!
//! A registry record says what exists. It cannot say what is happening, because a
//! lifecycle operation changes host state over tens or hundreds of milliseconds and
//! a crash in the middle leaves the two disagreeing. The journal is the third
//! record that closes that gap: an operation writes a record before it touches the
//! host and updates it as it goes, so a reader can tell what the daemon was doing
//! and whether it finished.
//!
//! The modules are split by what they hold. `record` is the shape of one operation
//! and the transitions it goes through. `journal` is the type a lifecycle operation
//! uses to write its own record. `store` is the directory: reading records back,
//! closing the ones a dead daemon left open, and keeping the directory bounded.
//! `current` is the operation id of the request the calling thread is serving, which
//! is what lets the CLI, the logs, and the journal name the same operation without
//! threading an id through every signature.

mod current;
mod journal;
mod record;
mod store;

pub use current::{current, is_valid_id, new_id, set_current};
pub use journal::Journal;
pub use record::{OperationRecord, OperationStatus};
#[cfg(test)]
pub(crate) use store::JOURNAL_TERMINAL_LIMIT;
pub use store::{close_unfinished_records, latest, load, prune_terminal_records};

#[cfg(test)]
#[path = "../../tests/src/operation/mod.rs"]
mod tests;
