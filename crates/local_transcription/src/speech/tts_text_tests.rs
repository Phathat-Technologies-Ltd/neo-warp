use super::*;

#[test]
fn rewrites_technical_tokens() {
    let cases = [
        ("cargo build --release", "cargo build dash dash release"),
        ("rm -rf target", "rm dash r f target"),
        ("--no-fail-fast", "dash dash no fail fast"),
        ("--color=always", "dash dash color equals always"),
        (
            "Edit app/src/lib.rs now.",
            "Edit app slash src slash lib dot rs now.",
        ),
        ("Open crates/warp_tui/src/view/mod.rs.", "Open mod dot rs."),
        ("cd ~/dev", "cd home slash dev"),
        ("Reverted 3f9a2c41be7.", "Reverted commit 3 f 9 a 2 c 4."),
        ("The API returned JSON.", "The A P I returned JSON."),
        ("Call parseConfig first", "Call parse Config first"),
        ("set max_width to 100", "set max width to 100"),
        ("Check Cargo.toml", "Check Cargo dot toml"),
        ("make && make test", "make and make test"),
        (
            "See https://github.com/warpdotdev/warp",
            "See a link to github dot com",
        ),
        ("Use `HTTPServer` here", "Use H T T P Server here"),
        ("offset -1", "offset minus 1"),
        ("cd ../..", "cd up slash up"),
    ];
    for (input, expected) in cases {
        assert_eq!(normalize(input), expected, "input: {input}");
    }
}

#[test]
fn drops_markdown_only_tokens_but_keeps_punctuation() {
    assert_eq!(normalize("Done -> next."), "Done next.");
    assert_eq!(normalize("**Note**: it works"), "Note: it works");
}
