use super::*;

#[test]
fn vocabulary_includes_directory_git_history_and_screen_terms() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pyproject.toml"), "").unwrap();
    std::fs::create_dir_all(dir.path().join(".git/refs/heads/feature")).unwrap();
    std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(dir.path().join(".git/refs/heads/feature/voice-fix"), "").unwrap();
    std::fs::write(
        dir.path().join(".git/config"),
        "[remote \"upstream\"]\n\turl = x\n",
    )
    .unwrap();
    let mut pane = PaneContext {
        cwd: Some(dir.path().to_owned()),
        history: vec!["kubectl get pods".to_owned()],
        ..PaneContext::default()
    };
    pane.push_screen_text("built atm-design-analyzer in 2s");

    let vocabulary = build_vocabulary(&pane);
    for term in [
        "pyproject.toml",
        "main",
        "feature/voice-fix",
        "upstream",
        "kubectl",
        "kubectl get",
        "atm-design-analyzer",
    ] {
        assert!(vocabulary.contains(term), "missing {term}");
    }
    assert!(vocabulary.len() <= context_correction::MAX_VOCABULARY_TERMS);
}

#[test]
fn screen_text_keeps_the_tail() {
    let mut pane = PaneContext::default();
    let text = format!("{}tail-term", "x ".repeat(MAX_SCREEN_BYTES));
    pane.push_screen_text(&text);
    assert!(pane.screen_text.len() <= MAX_SCREEN_BYTES + 1);
    assert!(pane.screen_text.ends_with("tail-term"));
}

fn correction(heard: &str, corrected: &str) -> (Correction, u64) {
    (
        Correction {
            heard: heard.to_owned(),
            corrected: corrected.to_owned(),
            range: 0..corrected.len(),
            confidence: 1.,
            pinned: false,
        },
        1,
    )
}

#[test]
fn one_compact_toast_per_utterance_and_none_for_trivial_changes() {
    assert_eq!(corrections_toast_text(&[]), None);
    assert_eq!(
        corrections_toast_text(&[correction("can you", "Can You")]),
        None
    );
    assert_eq!(
        corrections_toast_text(&[correction("hey, hello", "Hey hello")]),
        None
    );
    assert_eq!(
        corrections_toast_text(&[
            correction("pie project dot toml", "pyproject.toml"),
            correction("cube control", "kubectl"),
        ])
        .as_deref(),
        Some("Corrected pyproject.toml, kubectl")
    );
}
