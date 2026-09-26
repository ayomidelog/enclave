//! Bringing a registry read from disk up to the schema this binary writes.
//!
//! The registry is the only durable description of what exists, so a binary that
//! reads a record written by an older release has to either understand it or
//! refuse it. It must never guess: a field it silently drops is a sandbox the
//! operator can no longer see.
//!
//! Every supported older schema therefore gets one step here, in order, and the
//! current version is one past the last step. A step transforms the record one
//! version forward. A version whose shape did not change still gets a step, so
//! the table states that the change was considered rather than leaving the reader
//! to infer it from the absence of code.
//!
//! Adding a version means adding exactly one step. A registry whose version has
//! no step is an error, not a passthrough, which is what stops a future release
//! from silently reading a shape it was never written to handle.
//!
//! Migration is in memory. The migrated record is written by the next mutation,
//! which already persists the whole registry atomically, so a read-only command
//! never rewrites the file and an interrupted write leaves the previous record
//! intact.

use anyhow::{bail, Result};

use super::{Registry, REGISTRY_VERSION};

/// One schema step that was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MigrationStep {
    pub(crate) from: u32,
    pub(crate) to: u32,
}

/// Bring `registry` up to the schema this binary writes.
///
/// Returns the steps that were applied, in order, so a caller can report what
/// changed. A registry already at the current version returns an empty list.
pub(crate) fn migrate(registry: &mut Registry) -> Result<Vec<MigrationStep>> {
    if registry.version > REGISTRY_VERSION {
        // Not reachable through the load path, which rejects this first, but a
        // migration table should not be the thing that discovers it.
        bail!(
            "registry schema version {} is newer than this binary supports ({}); upgrade Enclave before using this state",
            registry.version,
            REGISTRY_VERSION
        );
    }

    let mut applied = Vec::new();
    while registry.version < REGISTRY_VERSION {
        let from = registry.version;
        match from {
            // Version 0 is a registry written before the version field was
            // enforced, so its shape is already the version 1 shape. The step
            // exists to record that, and to give the next real change a place to
            // live.
            0 => {}
            other => bail!(
                "no migration from registry schema version {other} to {}; this binary cannot read that record safely",
                other + 1
            ),
        }
        registry.version = from + 1;
        applied.push(MigrationStep { from, to: from + 1 });
    }
    Ok(applied)
}
