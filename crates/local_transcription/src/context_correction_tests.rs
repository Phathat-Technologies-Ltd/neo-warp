use super::*;

fn vocabulary() -> Vocabulary {
    let mut vocabulary = Vocabulary::default();
    vocabulary.extend([
        "atm-design-analyzer",
        "pyproject.toml",
        "git",
        "git status",
        "kubectl",
        "Cargo.toml",
        "README.md",
        "main",
    ]);
    vocabulary
}

fn fix(text: &str) -> String {
    correct(
        text,
        &vocabulary(),
        &CorrectionLog::default(),
        DEFAULT_THRESHOLD,
    )
    .text
}

#[test]
fn corrects_joined_forms() {
    assert_eq!(fix("cd atm design analyzer"), "cd atm-design-analyzer");
}

#[test]
fn corrects_phonetic_spellings() {
    assert_eq!(fix("open pie project dot toml"), "open pyproject.toml");
    assert_eq!(fix("get status"), "git status");
    assert_eq!(fix("cube control get pods"), "kubectl get pods");
}

#[test]
fn reports_heard_words_and_ranges() {
    let result = correct(
        "run get status.",
        &vocabulary(),
        &CorrectionLog::default(),
        DEFAULT_THRESHOLD,
    );
    assert_eq!(result.text, "run git status.");
    let [correction] = result.corrections.as_slice() else {
        panic!("expected one correction: {result:?}");
    };
    assert_eq!(correction.heard, "get status");
    assert_eq!(&result.text[correction.range.clone()], "git status");
}

#[test]
fn leaves_ordinary_speech_and_weak_matches() {
    assert_eq!(fix("get the logs"), "get the logs");
    assert_eq!(
        fix("explain this error please"),
        "explain this error please"
    );
    assert_eq!(fix("git status"), "git status");
}

#[test]
fn threshold_controls_replacement() {
    let log = CorrectionLog::default();
    let strict = correct("get status", &vocabulary(), &log, 0.99);
    assert_eq!(strict.text, "get status");
    assert!(strict.corrections.is_empty());
    let normal = correct("get status", &vocabulary(), &log, DEFAULT_THRESHOLD);
    assert_eq!(normal.text, "git status");
}

#[test]
fn pinned_mappings_apply_first() {
    let mut log = CorrectionLog::default();
    log.pin("get status", "gst", 1);
    let result = correct("get status", &vocabulary(), &log, DEFAULT_THRESHOLD);
    assert_eq!(result.text, "gst");
    assert!(result.corrections[0].pinned);

    log.pin("Warp Term", "warpterm", 2);
    let result = correct(
        "open warp term",
        &Vocabulary::default(),
        &log,
        DEFAULT_THRESHOLD,
    );
    assert_eq!(result.text, "open warpterm");
}

#[test]
fn reverted_mappings_are_suppressed() {
    let mut log = CorrectionLog::default();
    log.record_correction("get status", "git status", 1);
    log.record_reverted("get status", "git status", 2);
    assert_eq!(
        correct("get status", &vocabulary(), &log, DEFAULT_THRESHOLD).text,
        "get status"
    );
    log.record_accepted("get status", "git status", 3);
    log.record_accepted("get status", "git status", 4);
    assert_eq!(
        correct("get status", &vocabulary(), &log, DEFAULT_THRESHOLD).text,
        "git status"
    );
}

#[test]
fn disabled_and_removed_mappings() {
    let mut log = CorrectionLog::default();
    log.pin("get status", "git status", 1);
    log.set_disabled("get status", "git status", true, 2);
    assert_eq!(
        correct("get status", &vocabulary(), &log, DEFAULT_THRESHOLD).text,
        "get status"
    );
    log.remove("get status", "git status");
    assert!(log.entries().is_empty());
    assert_eq!(
        correct("get status", &vocabulary(), &log, DEFAULT_THRESHOLD).text,
        "git status"
    );
}

#[test]
fn log_round_trips_through_json() {
    let mut log = CorrectionLog::default();
    log.record_correction("cube control", "kubectl", 5);
    log.record_accepted("cube control", "kubectl", 6);
    log.pin("pie project", "pyproject.toml", 7);
    let restored = CorrectionLog::from_json(&log.to_json()).unwrap();
    assert_eq!(restored, log);
    assert_eq!(
        restored.pinned_target("pie project"),
        Some("pyproject.toml")
    );
}

#[test]
fn vocabulary_is_capped_and_deduplicated() {
    let mut vocabulary = Vocabulary::with_cap(3);
    assert!(vocabulary.add("main.rs"));
    assert!(!vocabulary.add("main.rs"));
    assert!(!vocabulary.add("x"));
    assert!(!vocabulary.add("1234"));
    vocabulary.extend(["a.rs", "b.rs", "c.rs"]);
    assert_eq!(vocabulary.len(), 3);
    assert!(!vocabulary.contains("c.rs"));
}

#[test]
fn extracts_identifier_tokens() {
    let tokens: Vec<&str> = identifier_tokens(
        "Edited `src/main.rs` and atm-design-analyzer, see https://x.io. Also fooBar ok.",
    )
    .collect();
    assert_eq!(tokens, ["src/main.rs", "atm-design-analyzer", "fooBar"]);
}

#[test]
fn phonetic_keys_merge_similar_sounds() {
    assert_eq!(phonetic_key("pieproject"), phonetic_key("pyproject"));
    assert_eq!(phonetic_key("kubecontrol"), phonetic_key("cubecontrol"));
    assert_ne!(phonetic_key("cargo"), phonetic_key("python"));
}
