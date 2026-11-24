use super::*;

fn rules(text: &str) -> String {
    remove_disfluencies(text, &[])
}

/// A scorer that removes the words at the given indices.
struct Marks(&'static [usize]);

impl DisfluencyScorer for Marks {
    fn delete_probabilities(&self, words: &[&str]) -> Result<Vec<f32>, TranscribeError> {
        Ok((0..words.len())
            .map(|i| if self.0.contains(&i) { 1.0 } else { 0.0 })
            .collect())
    }
}

struct Failing;

impl DisfluencyScorer for Failing {
    fn delete_probabilities(&self, _: &[&str]) -> Result<Vec<f32>, TranscribeError> {
        Err(TranscribeError::Engine("boom".to_owned()))
    }
}

#[test]
fn removes_fillers_and_fixes_commas_and_capitals() {
    assert_eq!(
        rules("Um, can you list the files?"),
        "Can you list the files?"
    );
    assert_eq!(rules("Open the, uh, file."), "Open the file.");
    assert_eq!(rules("Uh, uh, what's this?"), "What's this?");
    assert_eq!(
        rules("How many lines are in, uh, this file?"),
        "How many lines are in this file?"
    );
    assert_eq!(rules("Is it done, um?"), "Is it done?");
    assert_eq!(rules("Done. Um, what next?"), "Done. What next?");
}

#[test]
fn keeps_mm_after_a_number() {
    assert_eq!(rules("Make it 5 mm wider."), "Make it 5 mm wider.");
}

#[test]
fn removes_repeated_function_words_and_phrases() {
    assert_eq!(
        rules("What what is using the port?"),
        "What is using the port?"
    );
    assert_eq!(rules("Run the the tests."), "Run the tests.");
    assert_eq!(rules("Show me, show me the diff."), "Show me the diff.");
    assert_eq!(
        rules("Install the, uh, the latest node."),
        "Install the latest node."
    );
    assert_eq!(
        rules("Go back to the, go back to the previous directory."),
        "Go back to the previous directory."
    );
}

#[test]
fn keeps_repeated_content_words_and_sentences() {
    assert_eq!(
        rules("Echo hello hello into it."),
        "Echo hello hello into it."
    );
    assert_eq!(rules("Say bye bye."), "Say bye bye.");
    assert_eq!(rules("I know that that works."), "I know that that works.");
    assert_eq!(rules("Is it? Is it really?"), "Is it? Is it really?");
}

#[test]
fn removes_cut_off_fragments() {
    assert_eq!(
        rules("Please st start the server."),
        "Please start the server."
    );
    assert_eq!(rules("Can you fi find it?"), "Can you find it?");
    assert_eq!(rules("g- g- give me that"), "give me that");
}

#[test]
fn keeps_short_words_that_prefix_the_next() {
    assert_eq!(rules("I in fact did."), "I in fact did.");
    assert_eq!(rules("Go to today's log."), "Go to today's log.");
    assert_eq!(rules("A another one."), "A another one.");
}

#[test]
fn removes_comma_set_discourse_markers() {
    assert_eq!(rules("You know, I think it broke."), "I think it broke.");
    assert_eq!(rules("I mean, the server crashed."), "The server crashed.");
    assert_eq!(rules("It was, like, slow."), "It was slow.");
}

#[test]
fn keeps_discourse_words_that_carry_meaning() {
    for text in [
        "Do you know where the config is?",
        "I'd like to see the log.",
        "Add a retry so that it doesn't fail.",
        "I mean what I said.",
        "I like the layout, keep it.",
        "Make it look like the mockup.",
    ] {
        assert_eq!(rules(text), text);
    }
}

#[test]
fn never_touches_shell_syntax_or_quotes() {
    for text in [
        "Run git commit -m \"fix the the typo\".",
        "Run cd .. and then ls.",
        "Use rm -rf on um_dir.",
        "Open src/main.rs and uh.rs.",
        "Run cd dot dot.",
    ] {
        assert_eq!(rules(text), text);
    }
}

#[test]
fn protected_terms_are_kept() {
    let protected = vec!["hmm tool".to_owned()];
    assert_eq!(
        remove_disfluencies("Open hmm tool now.", &protected),
        "Open hmm tool now."
    );
}

#[test]
fn model_removes_restarts_but_not_locked_words() {
    assert_eq!(
        clean(
            "Go to the, uh, open the file.",
            &[],
            Some(&Marks(&[0, 1, 2])),
            0.5
        ),
        "Open the file."
    );
    assert_eq!(
        clean("Run npm run build.", &[], Some(&Marks(&[1, 2])), 0.5),
        "Run npm run build."
    );
    assert_eq!(
        clean("Open src/lib.rs now.", &[], Some(&Marks(&[1])), 0.5),
        "Open src/lib.rs now."
    );
    assert_eq!(
        clean("Then go up .. now.", &[], Some(&Marks(&[3])), 0.5),
        "Then go up .. now."
    );
}

#[test]
fn model_may_not_remove_a_lone_function_word() {
    assert_eq!(
        clean(
            "Run git commit and fix of the typo",
            &[],
            Some(&Marks(&[5])),
            0.5
        ),
        "Run git commit and fix of the typo"
    );
    // A repeated function word is a stutter.
    assert_eq!(
        clean(
            "Install the, the latest node.",
            &[],
            Some(&Marks(&[1])),
            0.5
        ),
        "Install the latest node."
    );
    // Part of a longer restart span.
    assert_eq!(
        clean(
            "Copy the file to move it to docs.",
            &[],
            Some(&Marks(&[0, 1, 2, 3])),
            0.5
        ),
        "Move it to docs."
    );
}

#[test]
fn model_removals_need_a_restart_or_discourse_role() {
    // Nothing after "I said" echoes it: the words are meant.
    assert_eq!(
        clean(
            "I mean what I said, delete the branch.",
            &[],
            Some(&Marks(&[3, 4])),
            0.5
        ),
        "I mean what I said, delete the branch."
    );
    assert_eq!(
        clean("That that is the problem.", &[], Some(&Marks(&[0])), 0.5),
        "That that is the problem."
    );
    assert_eq!(
        clean("So, why did it fail?", &[], Some(&Marks(&[0])), 0.5),
        "Why did it fail?"
    );
    assert_eq!(
        clean(
            "I mean the second file, not the first.",
            &[],
            Some(&Marks(&[0, 1])),
            0.5
        ),
        "I mean the second file, not the first."
    );
}

#[test]
fn model_that_removes_too_much_is_ignored() {
    assert_eq!(
        clean("Um, open the file.", &[], Some(&Marks(&[1, 2, 3])), 0.5),
        "Open the file."
    );
}

#[test]
fn failing_model_falls_back_to_rules() {
    assert_eq!(
        clean("Uh, run the the tests.", &[], Some(&Failing), 0.5),
        "Run the tests."
    );
}

#[test]
fn empty_and_all_filler_input() {
    assert_eq!(rules(""), "");
    assert_eq!(rules("Um."), "");
    assert_eq!(rules("Uh, um."), "");
}

#[test]
fn wordpiece_matches_bert() {
    let vocab = "[PAD]\n[UNK]\n[CLS]\n[SEP]\nhello\n,\nun\n##aff\n##able\nit\n'\ns\n";
    let wp = WordPiece::from_vocab_txt(vocab).unwrap();
    assert_eq!(wp.word_ids("Hello,"), vec![4, 5]);
    assert_eq!(wp.word_ids("unaffable"), vec![6, 7, 8]);
    assert_eq!(wp.word_ids("It's"), vec![9, 10, 11]);
    assert_eq!(wp.word_ids("xyz"), vec![1]);
    assert_eq!((wp.cls(), wp.sep()), (2, 3));
}

#[cfg(feature = "disfluency")]
#[test]
fn bundled_tagger_cleans_a_restart_and_keeps_a_control() {
    let tagger = crate::DisfluencyTagger::load().expect("load tagger");
    let clean = |text| clean(text, &[], Some(&tagger), DEFAULT_THRESHOLD);
    assert_eq!(
        clean("So uh what gi give me a brief on what did we look on."),
        "Give me a brief on what did we look on."
    );
    assert_eq!(
        clean("Do you know where the config file is?"),
        "Do you know where the config file is?"
    );
    assert_eq!(
        clean("Go to the uh open the file main RS"),
        "Open the file main RS"
    );
    assert_eq!(
        clean("Run git commit and fix of the typo"),
        "Run git commit and fix of the typo"
    );
}
