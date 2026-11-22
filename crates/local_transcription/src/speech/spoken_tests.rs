use super::*;

#[test]
fn splits_the_spoken_block_from_the_display_text() {
    let (spoken, display) =
        split_spoken_block("<spoken>The build passes now.</spoken>\n\nRan `cargo check -p app`.");
    assert_eq!(spoken.as_deref(), Some("The build passes now."));
    assert_eq!(display, "Ran `cargo check -p app`.");

    let (spoken, display) = split_spoken_block("No summary here.");
    assert_eq!(spoken, None);
    assert_eq!(display, "No summary here.");
}

#[test]
fn spoken_block_streams_without_flashing_the_tag() {
    assert_eq!(split_spoken_block("<spo"), (Some(String::new()), ""));
    assert_eq!(
        split_spoken_block("<spoken>I fixed the test.</spo"),
        (Some("I fixed the test.".to_owned()), "")
    );
    assert_eq!(
        split_spoken_block("<spoken>I fixed the test.</spoken>\nDet"),
        (Some("I fixed the test.".to_owned()), "Det")
    );
    assert_eq!(split_spoken_block("<div>"), (None, "<div>"));
}

#[test]
fn fallback_drops_paths_commands_and_code() {
    let reply = "I updated `app/src/voice/conversation.rs` to fix the bug.\n\n\
                 ```rust\nfn main() {}\n```\n\n\
                 Run cargo nextest run -p warp --features voice_input to check. Then push.";
    assert_eq!(
        fallback_readout(reply, true),
        "I updated to fix the bug. Run cargo nextest run warp to check."
    );
}

#[test]
fn fallback_drops_urls_hashes_lists_tables_and_long_numbers() {
    let reply = "Commit 3f9a2b7c landed, see https://github.com/x/y for details.\n\
                 - first item\n- second item\n\
                 | a | b |\n|---|---|\n\
                 It took 1234567 ms.";
    assert_eq!(
        fallback_readout(reply, true),
        "Commit landed, see for details. It took ms."
    );
}

#[test]
fn fallback_keeps_two_sentences_and_caps_words() {
    assert_eq!(fallback_readout("One. Two. Three.", true), "One. Two.");
    let long = "word ".repeat(100);
    assert_eq!(
        fallback_readout(&long, true).split_whitespace().count(),
        FALLBACK_MAX_WORDS
    );
}

#[test]
fn fallback_holds_back_a_partial_word_while_streaming() {
    assert_eq!(
        fallback_readout("Editing the file at app", false),
        "Editing the file at"
    );
    assert_eq!(
        fallback_readout("Editing the file. ", false),
        "Editing the file."
    );
    assert_eq!(
        fallback_readout("Here it is:\n```sh\nrm -rf", false),
        "Here it is:"
    );
}

#[test]
fn fallback_drops_unformatted_command_arguments() {
    assert_eq!(
        fallback_readout(
            "I ran find . -type d -not -path '*/.git*' -not -path '*/node_modules*' to list them.",
            true
        ),
        "I ran find to list them."
    );
}

#[test]
fn plain_speech_drops_shell_math_and_long_numbers() {
    assert_eq!(
        plain_speech("I set cutoff=$(( $(date +%s) - 7*86400 )) and searched."),
        "I set and searched."
    );
    assert_eq!(
        plain_speech("Ran find . -type d -not -path '*/.git*' to list folders."),
        "Ran find to list folders."
    );
    assert_eq!(plain_speech("`ls -la` | grep x"), "grep");
    assert_eq!(plain_speech("$(( 1 + 2 ))"), "");
    assert_eq!(
        plain_speech("Ran a find command: 14 folders."),
        "Ran a find command: 14 folders."
    );
    assert_eq!(plain_speech("See https://x.io/a now."), "See now.");
}
