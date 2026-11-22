use super::*;

#[test]
fn common_prefix_respects_char_boundaries() {
    assert_eq!(common_prefix_len("hello", "hello world"), 5);
    assert_eq!(common_prefix_len("hello world", "hello"), 5);
    assert_eq!(common_prefix_len("abc", "abd"), 2);
    assert_eq!(common_prefix_len("café ok", "café no"), 6);
    assert_eq!(common_prefix_len("é", "è"), 0);
    assert_eq!(common_prefix_len("", "x"), 0);
}
