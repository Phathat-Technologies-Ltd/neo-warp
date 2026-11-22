use super::*;

#[test]
fn normalizes_spoken_terminal_syntax() {
    let cases = [
        (
            "cargo test dash dash no fail fast",
            "cargo test --no-fail-fast",
        ),
        ("Dash dash no fail fast.", "--no-fail-fast"),
        ("tilde slash dev slash warp", "~/dev/warp"),
        ("cd tilde slash dev", "cd ~/dev"),
        ("dot rs", ".rs"),
        ("open main dot rs", "open main.rs"),
        ("git check out main", "git checkout main"),
        ("Git check out origin main.", "git checkout origin main"),
        ("check out this file", "check out this file"),
        ("git commit dash m fix", "git commit -m fix"),
        (
            "cargo test dash p warp dash dash no fail fast",
            "cargo test -p warp --no-fail-fast",
        ),
        ("cd dot dot", "cd .."),
        ("ls slash tmp", "ls /tmp"),
        ("ls pipe grep foo", "ls | grep foo"),
        ("echo hi greater than out dot txt", "echo hi > out.txt"),
        (
            "export rust underscore log equals debug",
            "export rust_log=debug",
        ),
        ("git cherry pick HEAD", "git cherry-pick HEAD"),
        ("git rebase origin main", "git rebase origin main"),
        ("Show me the logs, please.", "show me the logs please"),
    ];
    for (spoken, expected) in cases {
        assert_eq!(normalize(spoken), expected, "{spoken:?}");
    }
}

#[test]
fn keeps_identifiers_casing() {
    assert_eq!(normalize("git reset HEAD"), "git reset HEAD");
    assert_eq!(normalize("HEAD is detached"), "HEAD is detached");
}
