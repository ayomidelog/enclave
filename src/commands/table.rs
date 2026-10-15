//! Shared helpers for the aligned tables the CLI prints.

/// The width of one column: the widest cell, never narrower than its heading.
///
/// Every table here sizes its columns from the data so a set of long names does not
/// break the alignment and a set of short ones does not leave a gap under the
/// heading.
pub(crate) fn column_width<T>(
    items: &[T],
    len_fn: impl Fn(&T) -> usize,
    min_width: usize,
) -> usize {
    items
        .iter()
        .map(&len_fn)
        .max()
        .unwrap_or(min_width)
        .max(min_width)
}
