# Screen-aware voice transcript correction: technical design

See PRODUCT.md for behavior.

## Matching (`crates/local_transcription/src/context_correction.rs`)

Pure Rust, no dependencies beyond serde, no models.

- `Vocabulary`: deduplicated, capped (`MAX_VOCABULARY_TERMS` = 2000) list of terms. Each term stores letter-only *spellings* for the ways it can be said: symbols silent (`atmdesignanalyzer`), symbols spoken (`pyprojectdottoml`), and common abbreviations expanded (`ctl` → `control`, `py` → `pie`, `cfg` → `config`, ...), plus a phonetic key per spelling.
- `phonetic_key`: a small Metaphone-like key: `ph`→f, `ck/c/q/k`→k, soft `c`→s, `x`→k, `z`→s, `v`→f, `sh/ch`→x, `th`→0; vowels after the first letter dropped; repeats collapsed.
- `correct(text, vocabulary, log, threshold)`: walks the words; at each position it first looks for a pinned mapping for the next 4..1 words, then scores every span length against every term. Confidence is the best of spelling similarity (normalized Levenshtein) and a sound match (equal keys → 0.9 + 0.1·spelling; keys within one edit for keys of 5+ → 0.8 + 0.1·spelling). The best span above `DEFAULT_THRESHOLD` (0.86) wins, longer on ties; single words need 0.92 and a key of 4+ letters. A span whose leading word doesn't improve the match is skipped so the term starts later ("cd atm design analyzer" keeps `cd`). Trailing sentence punctuation is kept. Returns the text and `Correction { heard, corrected, range, confidence, pinned }`.
- `CorrectionLog`: `LogEntry { heard, corrected, count, accepted, reverted, last_seen, pinned, disabled }` keyed by normalized heard words + term, JSON (de)serialized. `is_suppressed` = disabled, or reverted ≥ 1 and reverted ≥ accepted, unless pinned. Pinning unpins other targets of the same words.
- `identifier_tokens`: path/identifier-like words (contain `-_./` or camelCase, no URLs) from free text.

Complexity: O(words × 4 × terms × spellings) Levenshtein on short strings, which for 2k terms takes about a millisecond per utterance.

## App (`app/src/voice/context_correction.rs`)

- `PaneContext` (cwd, recent history, tail of screen text ≤ 16 KB) is gathered on the UI thread without I/O by `Input::refresh_voice_vocabulary`. The terminal model is read with `try_lock`, so a held lock skips the output rather than risking a deadlock.
- `build_vocabulary` runs in `ctx.spawn` (background): history commands and their identifiers, screen identifiers, directory entries (≤ 300), `.git/HEAD`, `.git/refs/heads` (≤ 100), remotes from `.git/config` (read directly, no git process), then `PATH` executables (cached in a `OnceLock` per run) until the cap. Only one rebuild is in flight per input. The result is stored on the input's `EditorView` as an `Arc<Vocabulary>`.
- `VoiceCorrections` singleton: loads `voice_corrections.json` from `warp_core::paths::data_dir()` in the background at startup and writes it in the background after each change; holds up to 20 revertable `PendingCorrection`s. `correct` marks earlier pending corrections accepted, then logs new ones. `revert(id)` logs the revert and emits `Revert`, which every subscribed `EditorView` handles by replacing the last occurrence of the corrected term with the heard words.
- `EditorView` (`app/src/editor/view/voice_correction.rs`): `set_voice_vocabulary`, `correct_voice_transcript`, `show_voice_corrections` (blue underline via `update_buffer_styles` + a toast with a `WorkspaceAction::RevertVoiceCorrection(id)` link). Click-to-record corrects in `apply_transcribed_voice_input`; hands-free corrects in `Input::handle_hands_free_event` after `final_text_for_input`, so ranges match the inserted text.
- Setting `agents.voice.context_correction` (`VoiceContextCorrection`, default true); `is_enabled` also requires `FeatureFlag::VoiceInput`.
- Settings > Voice (`warp_agent_page.rs`): toggle, top 15 log entries with Pin/Disable/Delete, Export JSON.

## Follow-ups

- Identifiers from the last agent reply (needs an accessor from the agent view's latest exchange) and shell aliases (needs the session's alias table).
- Hover tooltip and click-to-revert on the underlined word itself, which needs editor support for per-range hover/click.
- The underline is cleared when input decorations are recomputed after an edit.

## Testing

- `crates/local_transcription/src/context_correction_tests.rs`: phonetic and joined-form examples (atm-design-analyzer, pyproject.toml, git status, kubectl), threshold behaviour, ordinary speech left alone, pinned precedence, revert suppression and recovery after accepts, disable/delete, JSON round trip, vocabulary caps/dedup, identifier extraction.
- `app/src/voice/context_correction_tests.rs`: vocabulary from a temp directory with a fake `.git`, history, and screen text; screen-text tail cap.
