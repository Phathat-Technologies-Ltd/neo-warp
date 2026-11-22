use super::*;

const HOOK: &str = "\x1bP$d7b22686f6f6b223a22507265636d64227d\x1b\\";

#[test]
fn strips_warp_dcs_hooks() {
    assert_eq!(strip_warp_sequences(&format!("a{HOOK}b")), "ab");
}

#[test]
fn strips_warp_private_and_integration_oscs() {
    let text = "\x1b]133;A\x07$ \x1b]7;file:///tmp\x1b\\ls\x1b]9278;d;7b7d\x07\x1b]9279\x07\r\n";
    assert_eq!(strip_warp_sequences(text), "$ ls\r\n");
}

#[test]
fn keeps_titles_colors_and_hyperlinks() {
    let text = "\x1b]0;my title\x07\x1b[1;31mred\x1b[0m \x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\";
    assert_eq!(strip_warp_sequences(text), text);
}

#[test]
fn keeps_other_dcs_and_long_osc_codes() {
    let text = "\x1bPq#0\x1b\\\x1b]13337;x\x07\x1b]1337;y\x07";
    assert_eq!(strip_warp_sequences(text), text);
}

#[test]
fn handles_sequences_split_across_chunks_at_every_point() {
    let text = format!("one\x1b[32m{HOOK}two\x1b]133;B\x1b\\three\x1b]2;t\x07");
    let expected = "one\x1b[32mtwothree\x1b]2;t\x07";
    for split in 0..=text.len() {
        let mut filter = WarpSequenceFilter::default();
        let mut out = filter.push(&text[..split]);
        out.push_str(&filter.push(&text[split..]));
        out.push_str(&filter.flush());
        assert_eq!(out, expected, "split at {split}");
    }
}

#[test]
fn flush_returns_held_prefix_and_drops_unterminated_hook() {
    let mut filter = WarpSequenceFilter::default();
    assert_eq!(filter.push("x\x1b]"), "x");
    assert_eq!(filter.flush(), "\x1b]");
    assert_eq!(filter.push("y\x1bP$d7b"), "y");
    assert_eq!(filter.flush(), "");
}
