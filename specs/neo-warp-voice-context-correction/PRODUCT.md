# Screen-aware voice transcript correction

Speech-to-text often mishears terminal vocabulary: "get status" for `git status`, "cube control" for `kubectl`, "pie project dot toml" for `pyproject.toml`. The correction step fixes those spans using what the pane already shows, so dictated commands and prompts name real files, branches, and commands.

## Scope

Applies to hands-free voice input and click-to-record (push-to-talk) voice input in the terminal input, for both shell commands and agent prompts. Behind the existing `VoiceInput` feature flag and the `voice_input` build feature, and the Settings > Voice toggle "Correct Transcripts From Screen" (on by default).

## Behavior

1. Each terminal input keeps a vocabulary of up to 2,000 terms drawn from its pane:
   - entries of the current directory (top level, capped);
   - path- and identifier-like tokens in recent commands and the latest block output;
   - the current git branch, local branches, and remotes;
   - command names (and command plus subcommand) from history, and executables on `PATH`.
   The vocabulary refreshes when the input gains focus, when a block completes, when hands-free starts, and after each hands-free utterance. Refreshing never blocks the UI.
2. After transcription (and, in shell inputs, after spoken-syntax normalization), spans of one to four words are compared with the vocabulary by spelling and by sound, including joined forms ("atm design analyzer" → `atm-design-analyzer`) and spoken symbols ("dot", "slash"). A span is replaced only when the match is strong; weak matches are left as heard. Single words need a stronger match than phrases, and a word that already appears in the vocabulary is never changed.
3. A corrected word is underlined in the input, and a toast shows "`<corrected>` (heard: `<heard>`)" with a **Revert** link. Reverting restores the heard words in the input and counts as negative feedback.
4. A local corrections log records each heard → corrected mapping with how often it was made, kept, and reverted, and when it was last seen. A correction that is not reverted before the next correction counts as kept.
5. Settings > Voice lists the log (pinned first, then by count) with **Pin/Unpin** (personal dictionary: always map these words to this term), **Disable/Enable**, and **Delete**, plus **Export JSON**, which writes `warp-voice-corrections.json` to the downloads folder.
6. Pinned mappings apply before any vocabulary match. A mapping that has been reverted at least as often as it was kept, or that is disabled, is not applied.

## Non-goals

- No model, training, fine-tuning, or background learning job: corrections are lookups of the vocabulary, the counts, and the pinned dictionary.
- The log is never uploaded.
- Identifiers from the last agent reply and shell aliases are not in the vocabulary yet (see TECH.md).
- No hover tooltip on the underlined word itself; the "heard" text and revert live in the toast.
