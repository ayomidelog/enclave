//! The operation id of the request the current thread is serving.

use std::cell::RefCell;

use uuid::Uuid;

thread_local! {
    /// The operation id of the request this thread is currently serving.
    ///
    /// A daemon request is handled start to finish on one worker thread, so a
    /// thread-local is enough to let the lifecycle code name the same operation
    /// the caller was told about, without threading an id through every
    /// signature. A CLI thread never sets it, so a directly invoked lifecycle
    /// function still gets a fresh id.
    static CURRENT_OPERATION: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Make `id` the operation id for work started on this thread.
pub fn set_current(id: Option<String>) {
    CURRENT_OPERATION.with(|current| *current.borrow_mut() = id);
}

/// The operation id for work started on this thread, if any.
pub fn current() -> Option<String> {
    CURRENT_OPERATION.with(|current| current.borrow().clone())
}

/// A fresh operation id.
pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// Whether `id` is shaped like an operation id.
///
/// A caller may supply an id so a retry can be correlated with the attempt it
/// repeats. The value is used as a file name, so anything that is not a plain
/// UUID is refused and a fresh id is generated instead.
pub fn is_valid_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok()
}
