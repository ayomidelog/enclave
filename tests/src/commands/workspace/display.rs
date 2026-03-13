use super::*;

#[test]
fn column_width_uses_min_for_empty_items() {
    let items: Vec<String> = vec![];
    assert_eq!(column_width(&items, |s| s.len(), 10), 10);
}

#[test]
fn column_width_uses_max_item_length() {
    let items = vec!["short".to_string(), "much longer string".to_string()];
    assert_eq!(column_width(&items, |s| s.len(), 5), 18);
}

#[test]
fn column_width_respects_minimum() {
    let items = vec!["ab".to_string()];
    assert_eq!(column_width(&items, |s| s.len(), 10), 10);
}
