//! Workspace session hardening, in three independent parts.
//!
//! * [`capabilities`] is the policy that constrains what a process in the
//!   workspace may do: the capability bounding and permitted sets, and the
//!   seccomp filter that denies the syscalls which would let it reach back into
//!   the host.
//! * [`masking`] hides host kernel information from the workspace by binding an
//!   empty file or directory over the path.
//! * [`readonly`] makes the namespace's own mounts immutable and detaches the
//!   old root after the pivot.
//!
//! They are applied in sequence while the session is being set up, and each one
//! stands alone: the policy does not depend on which paths are masked, and the
//! remounts do not depend on the policy.

mod capabilities;
mod masking;
mod readonly;

pub(crate) use capabilities::{apply_exec_restrictions, apply_session_restrictions};
pub(crate) use masking::mask_runtime_paths;
pub(crate) use readonly::{detach_old_root, tighten_namespace_mounts};

#[cfg(test)]
#[path = "../../../../tests/src/workspace/session/security.rs"]
mod tests;
