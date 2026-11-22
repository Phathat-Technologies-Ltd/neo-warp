use super::*;

fn chunk_all(deltas: &[&str]) -> Vec<String> {
    let mut chunker = SentenceChunker::new(ChunkerOptions::default());
    let mut out = Vec::new();
    for delta in deltas {
        out.extend(chunker.push(delta));
    }
    out.extend(chunker.finish());
    out
}

#[test]
fn emits_each_sentence_as_soon_as_it_completes() {
    let mut chunker = SentenceChunker::new(ChunkerOptions::default());
    assert!(chunker.push("The build").is_empty());
    assert_eq!(
        chunker.push(" failed. Two tests"),
        vec!["The build failed."]
    );
    assert_eq!(chunker.push(" broke! Fix"), vec!["Two tests broke!"]);
    assert_eq!(chunker.finish(), vec!["Fix"]);
}

#[test]
fn replaces_code_blocks_with_a_summary() {
    let out = chunk_all(&[
        "Here is the fix:\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\nRun it.",
    ]);
    assert_eq!(
        out,
        vec![
            "Here is the fix:",
            "I wrote a code block in rust, 3 lines.",
            "Run it."
        ]
    );
}

#[test]
fn handles_fences_split_across_deltas() {
    let out = chunk_all(&["Try this.\n`", "``", "sh\ncargo", " test\n`", "``\nDone."]);
    assert_eq!(
        out,
        vec!["Try this.", "I wrote a code block in sh, 1 line.", "Done."]
    );
}

#[test]
fn code_summaries_can_be_turned_off() {
    let mut chunker = SentenceChunker::new(ChunkerOptions {
        read_code_summaries: false,
    });
    let mut out = chunker.push("Look:\n```\nx\n```\nOk.");
    out.extend(chunker.finish());
    assert_eq!(out, vec!["Look:", "Ok."]);
}

#[test]
fn unclosed_fence_is_summarized_at_the_end() {
    assert_eq!(
        chunk_all(&["```python\nprint(1)\nprint(2)\n"]),
        vec!["I wrote a code block in python, 2 lines."]
    );
}

#[test]
fn strips_markdown_and_speaks_list_items_separately() {
    let out = chunk_all(&[
        "## Summary\n- Fixed **two** bugs in `src/lib.rs`\n- See [the docs](https://x.dev)\n\n| a | b |\n|---|---|\n",
    ]);
    assert_eq!(
        out,
        vec![
            "Summary",
            "Fixed two bugs in src slash lib dot rs",
            "See the docs"
        ]
    );
}

#[test]
fn keeps_snake_case_and_numbered_lists() {
    let out = chunk_all(&["1. Set max_width to 100.\n2. Run it."]);
    assert_eq!(out, vec!["Set max width to 100.", "Run it."]);
}

#[test]
fn does_not_split_on_abbreviations_or_decimals() {
    assert_eq!(
        chunk_all(&["Use e.g. version 1.5 now. Then stop."]),
        vec!["Use e dot g. version 1.5 now.", "Then stop."]
    );
}
