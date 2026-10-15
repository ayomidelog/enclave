use super::output_to_string;

#[test]
fn persistent_output_decodes_lossy_utf8() {
    assert_eq!(output_to_string(vec![b'o', b'k']), "ok");
}
