use super::*;

fn vocab() -> PieceVocab {
    PieceVocab::from_pieces(
        [
            "<unk>", "▁um", "▁uh", "▁er", "▁what", "▁is", "brella", ",", ".", "-", "h", "▁the",
            "▁Um", "m", "▁hm", "<blk>",
        ]
        .map(str::to_owned)
        .to_vec(),
    )
}

fn english() -> FillerFilter {
    FillerFilter::new(
        FillerSuppression {
            penalty: 4.0,
            english: true,
        },
        &vocab(),
    )
}

fn drop_word(filter: &FillerFilter, tokens: &[u32], word: Range<usize>) -> Vec<u32> {
    let mut tokens = tokens.to_vec();
    filter.drop_filler_word(&mut tokens, word, &mut [], &vocab());
    tokens
}

#[test]
fn penalizes_whole_filler_pieces_only() {
    let filter = english();
    assert_eq!(filter.penalty(1), 4.0);
    assert_eq!(filter.penalty(12), 4.0);
    assert_eq!(filter.penalty(4), 0.0);
    assert_eq!(filter.penalty(13), 0.0);
}

#[test]
fn other_languages_keep_words_that_are_fillers_in_english() {
    let suppression = FillerSuppression {
        penalty: 4.0,
        ..FillerSuppression::for_language(None)
    };
    let filter = FillerFilter::new(suppression, &vocab());
    assert_eq!(filter.penalty(1), 0.0, "German 'um'");
    assert_eq!(filter.penalty(3), 0.0, "German 'er'");
    assert_eq!(filter.penalty(2), 4.0);
    assert!(!filter.is_filler("um"));
    assert!(filter.is_filler("uh,"));
    assert!(FillerSuppression::for_language(Some("en")).english);
}

#[test]
fn choose_takes_the_runner_up_only_when_the_penalty_flips_it() {
    let filter = english();
    let mut logits = vec![0.0; 16];
    logits[1] = 5.0;
    logits[15] = 3.0;
    assert_eq!(filter.choose(&logits, 1), 15);
    logits[1] = 9.0;
    assert_eq!(filter.choose(&logits, 1), 1);
    logits[4] = 10.0;
    assert_eq!(filter.choose(&logits, 4), 4);
}

#[test]
fn drops_filler_words_with_their_comma() {
    let filter = english();
    // "what um, is" -> "what is"
    assert_eq!(drop_word(&filter, &[4, 1, 7, 5], 1..3), vec![4, 5]);
    // "the, um, is" -> "the is"
    assert_eq!(drop_word(&filter, &[11, 7, 1, 7, 5], 2..4), vec![11, 5]);
    // "umm" spelled as two pieces.
    assert_eq!(drop_word(&filter, &[4, 1, 13, 5], 1..3), vec![4, 5]);
}

#[test]
fn keeps_sentence_end_after_a_dropped_filler() {
    assert_eq!(drop_word(&english(), &[4, 1, 8], 1..3), vec![4, 8]);
    assert_eq!(drop_word(&english(), &[12, 8], 0..2), Vec::<u32>::new());
}

#[test]
fn keeps_words_that_start_like_fillers_and_backchannels() {
    let filter = english();
    // "umbrella"
    assert_eq!(drop_word(&filter, &[1, 6], 0..2), vec![1, 6]);
    // "uh-huh"
    assert_eq!(drop_word(&filter, &[2, 9, 10, 2], 0..4), vec![2, 9, 10, 2]);
    assert!(!filter.is_filler("a"));
    assert!(!filter.is_filler("I"));
}

#[test]
fn keeps_fillers_inside_a_bias_match_and_shifts_later_matches() {
    let filter = english();
    let mut tokens = vec![4, 2, 5, 11];
    let mut matches = [BiasMatch {
        start: 3,
        end: 4,
        term: 0,
    }];
    assert!(filter.drop_filler_word(&mut tokens, 1..2, &mut matches, &vocab()));
    assert_eq!(tokens, vec![4, 5, 11]);
    assert_eq!((matches[0].start, matches[0].end), (2, 3));

    let mut inside = [BiasMatch {
        start: 0,
        end: 2,
        term: 0,
    }];
    let mut tokens = vec![4, 2, 5];
    assert!(!filter.drop_filler_word(&mut tokens, 1..2, &mut inside, &vocab()));
}

#[test]
fn capitalizes_sentences() {
    assert_eq!(capitalize_sentences("what is. it? ok"), "What is. It? Ok");
}
