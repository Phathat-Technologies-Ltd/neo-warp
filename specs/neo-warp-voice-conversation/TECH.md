# Voice Conversation Mode for neo-warp: Tech Spec

See `PRODUCT.md`.

## TTS model choice

| Model | Size | License | Quality | Notes |
|---|---|---|---|---|
| **Kokoro-82M v1.0 (chosen)** | int8 ONNX about 88 MB, plus voices about 27 MB | Apache-2.0 | Close to the top of TTS Arena for its size | 24 kHz output; first sentence in about 0.1 to 0.3 s on an M-series CPU (estimate) |
| **tiny-voca (fallback)** | about 18 to 21 MB ONNX, 4.5M params | Apache-2.0 | The author reports 8th place (M2 voice) and 13th (F2) of 50 on the Open TTS Leaderboard | Released Nov 2026; two US English voices; about 70x real time on 4 threads; no neural vocoder |
| KittenTTS (nano/mini) | 15 to 80 MB | Apache-2.0 | Clearly below Kokoro | A possible second fallback |
| Piper | 20 to 60 MB per voice | GPL-3 (piper1-gpl); voice licenses vary | Sounds robotic on technical text | Rejected because of the license |

- **Kokoro**: use `onnx-community/Kokoro-82M-v1.0-ONNX` (`onnx/model_quantized.onnx`), or the
  `thewh1teagle/kokoro-onnx` release files `kokoro-v1.0.int8.onnx` and `voices-v1.0.bin`. The
  default voice is `af_heart`. Inputs are phoneme token ids, a style vector, and a speed. It runs on
  `ort`.
- **tiny-voca**: `https://huggingface.co/n4ze3m/tiny-voca` (ONNX weights; source at
  `github.com/n4ze3m/tiny-voca`). It is the fallback for machines with little free disk, and it
  becomes the default if the Kokoro phonemizer (next section) is blocked by licensing.
- **Checksums**: each file is pinned in `ModelSpec` by:
  - a URL at a fixed revision (the Hugging Face commit hash in the path, `resolve/<sha>/...`),
  - its SHA-256,
  - its exact size.

  These values are computed during implementation and cross-checked against the Hugging Face LFS
  `oid`.

### Pronunciation and phonemes (G2P)
Kokoro takes phonemes as input. Its reference pipeline uses misaki with espeak-ng, and espeak-ng is
GPL-3, so we do not link it. The plan:
- Look words up in CMUdict (BSD-style license) and map them to Kokoro's IPA set.
- Use a small rule-based letter-to-sound fallback for words not in the dictionary.
- Add a terminal lexicon: git, sudo, nginx, kubectl, JSON, YAML, regex, stdout, and similar.

Before G2P, `tts_text::normalize` rewrites technical tokens:
- `--flag` becomes "dash dash flag", and `-rf` becomes "dash r f".
- Paths are split on `/` and `.`.
- All-caps acronyms are spelled out unless they are in the lexicon.
- Hex values and SHAs become "commit abc123" (first 7 characters).
- camelCase and snake_case identifiers are split into words.

If the CMUdict approach does not sound good enough, tiny-voca, which has its own text front end,
becomes the primary model.

## Architecture
- **`Speaker` trait** in `crates/local_transcription`, next to `Transcriber`:
  `fn synthesize(&self, text: &str, voice: &VoiceId, rate: f32) -> Result<Audio, SpeakError>`,
  plus `is_ready()` and `voices()`. Calls are blocking and run in `spawn_blocking`. Implementations
  sit behind features: `KokoroSpeaker` (`kokoro`) and `TinyVocaSpeaker` (`tiny_voca`).
- **`SentenceChunker`** (pure logic): takes the streamed text deltas, tracks markdown state (fenced
  code, inline code, lists), and emits sentences that can be spoken. It replaces code blocks with
  summaries.
- **Tool-call summaries**: tool-call blocks from the blocklist are summarized through a
  `SpokenSummary` trait. Each action type implements it, using an exhaustive match.
- **Client**: `app/src/voice/conversation.rs` holds a `ConversationSession` model that subscribes to
  the active AI block stream (`app/src/ai/blocklist`; the TUI version lives in `crates/warp_tui`).
  Each sentence is queued, synthesized, and played, with synthesis running one sentence ahead. The
  session state drives the indicators.
- **Where synthesis runs**: in the client process, for low latency and no extra network hop. The
  embedded server handles only STT. A `POST /ai/speak` route could be added later for the TUI in
  remote sessions.

## Audio output, echo cancellation, barge-in
- **macOS**: a single VoiceProcessingIO AudioUnit handles both input and output, so the system echo
  canceller has the playback signal as its reference. Settings:
  - subtype `kAudioUnitSubType_VoiceProcessingIO`,
  - `kAUVoiceIOProperty_BypassVoiceProcessing` = 0,
  - ducking of other audio turned off through `kAUVoiceIOProperty_OtherAudioDuckingConfiguration`.

  It is bound through `objc2-audio-toolbox` or `coreaudio-sys`. Hands-free capture and TTS playback
  both go through this unit.
- **Other platforms**: `cpal` output with no echo cancellation. Barge-in then needs headphones, or
  falls back to a higher VAD threshold while the assistant is speaking plus a simple gate on the
  playback signal. WebRTC AEC3 (`webrtc-audio-processing` crate, BSD) is future work.
- **Barge-in**: if the VAD reports `speech_start` while Speaking and the speech lasts 120 ms, the
  session stops the output queue, drops pending sentences, and marks the reply as "interrupted" in
  its block.

## Approvals
When a blocklist action enters the approval-required state, the session speaks a short prompt. It
then listens for an approval phrase in the next final utterance: yes, approve, or run it; or no,
skip, or cancel. A match triggers the same action as the matching on-screen button. Other speech
does not affect the approval. Commands the existing risk classification marks as destructive still
need on-screen approval.

## Models and consent
`ModelSpec::kokoro_82m_v1_int8()` and `ModelSpec::tiny_voca()` are stored through `ModelStore`. Each
has its own `ModelState`, consent dialog, and `ProgressThrottle` toasts, plus a Settings > Voice >
Speech Output Model row.

## Flags
Add `FeatureFlag::VoiceConversation` to the org flags system (default ON in org builds) and document
it in `specs/neo-warp-flags`. It requires `VoiceHandsFree`, and all of its UI is hidden when it is
off.

## Testing
- Chunker tests, including streamed deltas and code fences split across deltas.
- Table tests for the text normalization.
- Tests for the approval phrase matching.
- Session tests with a fake `Speaker` that check barge-in ordering.
- TUI render-to-lines tests for the indicators.

## Risks
- Kokoro may sound worse with the CMUdict phonemizer than with espeak-ng.
- tiny-voca is new and has not been widely tested.
- VoiceProcessingIO changes the input device format and may lower the mic level.

## Implementation notes (v1)
- **Pinned files** (hashes checked against each Hugging Face LFS `oid`; `tokenizer.json`,
  `config.json`, and CMUdict are not LFS files and were hashed with `shasum -a 256`):

  | Model | File | Size | SHA-256 |
  |---|---|---|---|
  | Kokoro, `onnx-community/Kokoro-82M-v1.0-ONNX@1939ad2a` | `onnx/model_quantized.onnx` | 92,361,116 | `fbae9257…1478` |
  | | `tokenizer.json` | 3,497 | `77a02c8e…0a34` |
  | | `voices/af_heart.bin`, `af_bella`, `am_michael`, `am_adam` | 522,240 each | see `model.rs` |
  | tiny-voca, `n4ze3m/tiny-voca@5fe06641` | `onnx/model.onnx` | 21,386,372 | `fe6d9187…ae46` |
  | | `config.json` | 2,129 | `27bcbf32…f69c` |
  | CMUdict, `cmusphinx/cmudict@74790861` (both models) | `cmudict.dict` | 3,618,488 | `81917843…3d22` |

  The Kokoro bundle is 99 MB (four voices rather than all 54), and the consent toast shows the
  computed size rather than the estimate in `PRODUCT.md`.
- **Code layout**: `local_transcription::speech` holds the `Speaker` trait, `tts_text`, the
  `SentenceChunker`, `approval` (phrase matching and `ApprovalFlow`), `summary` (`SpokenSummary`
  and command-output summaries), `playback` (`ConversationController`, the pure state machine for
  the queue, one-ahead synthesis, barge-in, "stop", and approvals), and `g2p` (CMUdict plus a
  terminal lexicon and letter rules). `KokoroSpeaker` (feature `kokoro`) and `TinyVocaSpeaker`
  (feature `tiny_voca`) map the ARPAbet output to each model's phoneme set. Both were run against
  the real models (ignored `synthesizes_with_the_real_model` tests): a warm Kokoro sentence takes
  about 270 ms in a release build; loading takes a few seconds, so the session warms the model up
  when conversation mode starts.
- **App**: `app/src/voice/conversation.rs` (`ConversationSession`) drives the controller, runs
  synthesis on a thread, and plays through `voice_input::SpeechOutput` (cpal). It follows the
  `BlocklistAIHistoryModel` stream for the surface that sent the last prompt;
  `conversation_text.rs` turns an exchange's output into markdown for the chunker and implements
  `SpokenSummary` for tool calls (exhaustive match). `Input` forwards prompts sent and approval
  requests from its `BlocklistAIActionModel`. Hands-free hands conversation mode each final before
  applying it, so "stop" and approval answers never reach the input.
- **Models**: `VoiceModel<K>` generalizes the speech model's consent, progress, cancel, and toasts;
  `SpeechOutputModel` is the speech output model, with its own consent toast (size, live progress,
  Cancel link) and a Settings > Voice > Speech Output Model row with Download, Cancel, and Remove.
  Settings > Voice also has the engine, voice, rate (0.8x to 1.5x), and Read Code Summaries rows,
  shown only when `VoiceConversation` is on.
- **Toggle**: Command Palette entries and `cmd-alt-shift-V` (`ctrl-alt-shift-V` on Linux and
  Windows). Turning it on asks for the speech output model if needed and turns on hands-free.
- **Not done in v1**:
  - macOS VoiceProcessingIO echo cancellation. Output uses cpal on every platform, so barge-in
    works reliably only with headphones; the higher VAD threshold during playback is not done.
  - Command-output summaries ("Ran cargo check: 2 errors") are implemented
    (`summary::command_result_summary`) but not yet wired to action results; tool calls are
    announced when they start ("Running cargo check.").
  - Marking an interrupted reply in its block (it is logged), the footer Listening/Thinking/Speaking
    indicator (shown in the window header instead), the footer toggle button, and the TUI.
  - Commands without a risk classification other than `RequestCommandOutput` are announced and left
    to the on-screen buttons.
