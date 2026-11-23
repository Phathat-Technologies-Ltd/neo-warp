use super::*;

/// A tiny vocabulary: every lowercase and uppercase letter as a word start and a continuation,
/// plus a few whole words.
fn vocab() -> PieceVocab {
    let mut pieces = Vec::new();
    for c in ('a'..='z').chain('A'..='Z') {
        pieces.push(format!("{WORD_START}{c}"));
        pieces.push(c.to_string());
    }
    for piece in [
        "▁sa", "st", "▁SA", "ST", "▁AI", "-", "▁the", "▁is", "▁kube", "ctl", "▁kub", "ectl", ".",
        "▁last", "toml", "▁py", "project",
    ] {
        pieces.push(piece.to_owned());
    }
    pieces.push("<blk>".to_owned());
    PieceVocab::from_pieces(pieces)
}

fn id(vocab: &PieceVocab, piece: &str) -> u32 {
    (0..vocab.len() as u32)
        .find(|&id| vocab.piece(id) == piece)
        .unwrap_or_else(|| panic!("{piece} not in vocab"))
}

#[test]
fn surface_forms_cover_case_splits_spelled_acronyms_and_spoken_parts() {
    let forms = surface_forms("SAST-AI");
    for expected in ["sast-ai", "sast ai", "s a s t a i"] {
        assert!(
            forms.contains(&expected.to_owned()),
            "{expected} in {forms:?}"
        );
    }
    let forms = surface_forms("pyproject.toml");
    assert!(forms.contains(&"pyproject toml".to_owned()));
    assert!(forms.contains(&"pie project toml".to_owned()));
    assert!(!forms.iter().any(|form| form.contains("p y")));
    let forms = surface_forms("kubectl");
    assert!(forms.contains(&"cube ctl".to_owned()));
    assert!(forms.contains(&"kube control".to_owned()));
    assert_eq!(surface_forms("happy"), vec!["happy"]);
}

#[test]
fn tree_dedupes_terms_case_insensitively() {
    let tree = BoostingTree::build(
        &[BiasTerm::new("SAST-AI"), BiasTerm::new("sast-ai")],
        BiasConfig::default(),
    );
    assert_eq!(tree.term_count(), 1);
}

fn walk(tree: &BoostingTree, vocab: &PieceVocab, pieces: &[&str]) -> (f32, Vec<BiasMatch>) {
    let mut state = BiasState::default();
    let mut total = 0.0;
    let mut matches = Vec::new();
    for (position, piece) in pieces.iter().enumerate() {
        let token = id(vocab, piece);
        let bonus = tree.bonus(&state, token, -1.0, (token, -1.0), vocab);
        total += bonus.unwrap_or_else(|| -tree.leave_penalty(&state));
        let (next, found) = tree.advance(&state, token, position, bonus, vocab);
        state = next;
        matches.extend(found);
    }
    (total, matches)
}

#[test]
fn matched_tokens_earn_a_bonus_and_complete_a_match() {
    let vocab = vocab();
    let config = BiasConfig::default();
    let tree = BoostingTree::build(&[BiasTerm::new("kubectl")], config);
    let (total, matches) = walk(&tree, &vocab, &["▁the", "▁kube", "ctl", "▁is"]);
    assert_eq!(total, config.entry_weight + config.weight);
    assert_eq!(
        matches,
        vec![BiasMatch {
            start: 1,
            end: 3,
            term: 0
        }]
    );
}

#[test]
fn any_segmentation_and_case_matches() {
    let vocab = vocab();
    let tree = BoostingTree::build(&[BiasTerm::new("kubectl")], BiasConfig::default());
    for pieces in [
        &["▁kub", "ectl"][..],
        &["▁K", "u", "b", "e", "c", "t", "l"][..],
    ] {
        let (_, matches) = walk(&tree, &vocab, pieces);
        assert_eq!(matches.len(), 1, "{pieces:?}");
    }
}

#[test]
fn leaving_a_partial_match_takes_the_bonus_back() {
    let vocab = vocab();
    let tree = BoostingTree::build(&[BiasTerm::new("kubectl")], BiasConfig::default());
    let (total, matches) = walk(&tree, &vocab, &["▁kube", "▁is"]);
    assert_eq!(total, 0.0);
    assert!(matches.is_empty());
}

#[test]
fn terms_start_only_at_word_boundaries() {
    let vocab = vocab();
    let tree = BoostingTree::build(&[BiasTerm::new("ctl")], BiasConfig::default());
    let state = BiasState::default();
    let ctl = id(&vocab, "ctl");
    assert!(tree.bonus(&state, ctl, -0.1, (ctl, -0.1), &vocab).is_none());
}

#[test]
fn entry_is_gated_by_model_probability_and_common_words() {
    let vocab = vocab();
    let config = BiasConfig::default();
    let tree = BoostingTree::build(&[BiasTerm::new("kubectl")], config);
    let state = BiasState::default();
    let kube = id(&vocab, "▁kube");
    let the = id(&vocab, "▁the");
    let last = id(&vocab, "▁last");
    // Too unlikely to the model.
    assert!(
        tree.bonus(&state, kube, -9.0, (last, -0.5), &vocab)
            .is_none()
    );
    // Would replace a confidently predicted everyday word.
    assert!(
        tree.bonus(&state, kube, -2.0, (the, -0.05), &vocab)
            .is_none()
    );
    // Plausible, and the best guess isn't an everyday word.
    assert!(
        tree.bonus(&state, kube, -2.0, (last, -0.5), &vocab)
            .is_some()
    );
    // Trusted terms start from less likely tokens.
    let trusted = BoostingTree::build(&[BiasTerm::trusted("kubectl")], config);
    assert!(
        trusted
            .bonus(&state, kube, -5.0, (last, -0.5), &vocab)
            .is_some()
    );
    assert!(
        tree.bonus(&state, kube, -5.0, (last, -0.5), &vocab)
            .is_none()
    );
}

#[test]
fn matches_render_as_the_term_is_written() {
    let vocab = vocab();
    let tree = BoostingTree::build(&[BiasTerm::new("SAST-AI")], BiasConfig::default());
    let pieces = ["▁the", "▁sa", "st", "▁a", "i", "▁is"];
    let tokens: Vec<u32> = pieces.iter().map(|p| id(&vocab, p)).collect();
    let (_, matches) = walk(&tree, &vocab, &pieces);
    assert_eq!(
        render_with_matches(&tokens, &matches, &tree, &vocab).trim(),
        "the SAST-AI is"
    );
}

#[test]
fn detokenize_spaces_digits_after_words() {
    let vocab = PieceVocab::from_pieces(
        ["▁at", "60", "▁A", "4"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    );
    assert_eq!(detokenize(&[0, 1, 2, 3], &vocab), " at 60 A4");
}

#[test]
fn ranking_dedupes_filters_and_caps() {
    let candidate = |text: &str, score: f32, trusted: bool| BiasCandidate {
        text: text.to_owned(),
        score,
        trusted,
    };
    let ranked = rank_bias_terms(
        [
            candidate("kubectl", 1.0, false),
            candidate("Kubectl", 3.0, false),
            candidate("the", 9.0, false),
            candidate("SAST-AI", 2.0, true),
            candidate("ab", 5.0, false),
            candidate("Cerebras", 0.5, false),
        ],
        2,
    );
    assert_eq!(
        ranked,
        vec![BiasTerm::new("Kubectl"), BiasTerm::trusted("SAST-AI")]
    );
}

#[test]
fn bias_worthy_terms_are_uncommon() {
    assert!(is_bias_worthy("git rebase"));
    assert!(is_bias_worthy("atm-design-analyzer"));
    assert!(!is_bias_worthy("show me the"));
    assert!(!is_bias_worthy("https://example.com"));
}

#[test]
fn salient_tokens_pick_names_out_of_prose() {
    let tokens = salient_tokens(
        "I ran SAST-AI on `src/atm-design-analyzer` and Cerebras, then OpenAI checked it. \
         The pyproject.toml is fine. See https://example.com for more.",
    );
    assert_eq!(
        tokens,
        [
            "SAST-AI",
            "atm-design-analyzer",
            "Cerebras",
            "OpenAI",
            "pyproject.toml"
        ]
    );
}

#[test]
fn surface_forms_combine_word_alternatives() {
    let forms = surface_forms("kubectl");
    assert!(forms.contains(&"cube control".to_owned()), "{forms:?}");
    let forms = surface_forms("SAST-AI");
    assert!(forms.contains(&"sast a i".to_owned()), "{forms:?}");
    assert!(forms.contains(&"s a s t ai".to_owned()), "{forms:?}");
    assert!(forms.len() <= 24);
}
