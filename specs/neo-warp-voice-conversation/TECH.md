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
  - ducking of other audio set to the minimum through
    `kAUVoiceIOProperty_OtherAudioDuckingConfiguration`.

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
does not affect the approval. Commands the existing risk classification marks as destructive, or
does not classify, still need on-screen approval. See "Approval safety" below for the timing rules
of the answer window.

## Models and consent
`ModelSpec::kokoro_82m_v1_int8()` and `ModelSpec::tiny_voca()` are stored through `ModelStore`. Each
has its own `ModelState`, consent dialog, and `ProgressThrottle` toasts, plus a Settings > Voice >
Speech Output Model row.

## Flags
Add `FeatureFlag::VoiceConversation` to the org flags system (default ON in org builds) and document
it in `specs/neo-warp-flags`. It requires `VoiceHandsFree`, and all of its UI is hidden when it is
off.

## Never speaking command text

`speech::summary` builds every tool readout from `program_name` (first non-wrapper word, only when
it is a short plain name) and `command_description` (a plain effect for destructive programs such
as `rm`, `git push --force`, `kubectl apply`). `approval_prompt` speaks a subject phrase verbatim,
built by `spoken_action_prompt`. Tool calls and results are never spoken: `spoken_reply_text`
leaves actions out, and `conversation_text::action_announcement` returns a line only for
`AskUserQuestion` and `TransferShellCommandControlToUser`.
`spoken::plain_speech` runs in `ConversationController::enqueue` on every sentence before
synthesis. `ConversationSession` follows `HandsFreeSession::target_surface()`.

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
- **Approval safety** (`approval::ApprovalFlow`, `ConversationController`):
  - Only a `RequestCommandOutput` whose `is_risky` is `Some(false)` can be approved by voice.
    Risky (`Some(true)`) and unclassified (`None`) commands, and every other action, are announced
    with "Please approve it on screen" and no spoken phrase answers them.
  - Accepted phrases are whole utterances: yes, approve(d), run it, yes run it, go ahead; or no,
    nope, skip (it), cancel, reject, don't, do not. "Yeah" and "yep" are not accepted.
  - The answer window opens when the prompt sentence finishes playing (or its synthesis fails)
    plus `ECHO_TAIL` (700 ms), and only an utterance whose VAD `speech_start` is inside the window
    counts. A prompt that never finished playing (stopped, barged in, replaced) never opens it.
  - After `APPROVAL_LISTEN_FOR` (20 s) the window closes, the assistant says "I'll wait for you to
    approve it on screen", and only the buttons answer.
  - An approval phrase that can't answer (too early, too late, on-screen only) is dropped rather
    than sent to the agent as a prompt (`FinalOutcome::Ignored`).
- **Echo gate (instead of VoiceProcessingIO in this pass)**: output still uses cpal, with no echo
  cancellation, so the controller decides what to trust:
  - Output kind: `SpeechOutput` reports cpal's device type (Headphones, Headset, Earpiece,
    HearingAid) and name; `is_headphone_device_name` also matches names such as "AirPods",
    "Headphones", "Buds". When neither says headphones, the output is treated as loudspeakers.
  - Loudspeakers without echo cancellation are strictly half-duplex. A sustained echo of the reply
    used to pass the old 600 ms speaker barge-in, stop playback, and be sent as the next prompt, so
    the agent talked to itself. Now:
    - Speech never barges in (`check_barge_in` does nothing); only a stop phrase interrupts.
    - A final is echo (`FinalOutcome::Echo`, never reaching the input) when its utterance started
      while audio played or within `ECHO_TAIL` (700 ms) after it, when playback started while it
      was under way, or when it arrives inside that window without a speech start.
    - During that window (`ConversationController::is_echo_window`) hands-free shows empty
      partials and holds any pending auto-send, re-checking every 200 ms, so echo can neither show
      text nor send a follow-up turn.
  - With echo cancellation (`set_echo_cancelled(true)`), barge-in needs
    `BARGE_IN_AFTER_WITH_ECHO_CANCELLATION` (200 ms) and a barged-in final is used; overlapping
    speech that never barged in is still echo. On headphones, barge-in needs `BARGE_IN_AFTER`
    (120 ms) and overlapping speech is used. Overlap is measured from the later of the user's
    speech start and the start of playback, re-checked every `BARGE_IN_POLL` (40 ms).
  - Whatever the output, a final of 2+ words of which at least 80% appear in the last 8
    synthesized sentences, within `RECENT_SPEECH_WINDOW` (10 s) of the assistant last being
    audible, is dropped as echo. Approval answers are checked first, because they reuse the
    prompt's words ("Say run it, or no").
  - "Stop" (stop, stop talking, stop it, quiet, be quiet, shush) is a command only while the
    assistant is Speaking or Thinking, when it barged in, or when it overlapped the speech; while
    Listening it is an ordinary prompt. After a stop or barge-in, later replies and readouts of
    the same turn stay silent until the next prompt.
  - Not done: a playback-correlated energy gate and a raised VAD threshold during playback (the
    server VAD config is fixed per socket).
  - Real AEC is the `voice_input::voice_processing` backend described under "Echo cancellation
    (macOS voice-processing unit)" below; the conversation calls `set_echo_cancelled` from it.
- **No tool progress or results**: tool calls and command results are not spoken; the reply's
  `<spoken>` summary (or the server narration fallback) covers them.
  Follow-up replies of the same turn (after a tool call) are spoken too (`reply_started`).
- **Not done in v1**:
  - A Settings toggle and Command Palette entry for echo cancellation (TOML only for now), and
    echo cancellation outside macOS.
  - Marking an interrupted reply in its block (it is logged), the footer Listening/Thinking/Speaking
    indicator (shown in the window header instead), the footer toggle button, and the TUI.
  - Commands without a risk classification other than `RequestCommandOutput` are announced and left
    to the on-screen buttons.
- **Guided readout** (`local_transcription::speech::spoken`):
  - While conversation mode is on, the client sets `VOICE_CONVERSATION_ACTIVE`, and every agent
    request (including follow-ups after tool calls) carries `voice_conversation: true` in
    `Request.metadata.logging` (an existing free-form map, so no proto change). The embedded server
    (`selfhost_server::agent::prompt::wants_spoken_summary`) then appends instructions: open every
    reply with one or two plain-English sentences in `<spoken>…</spoken>`, with no paths, commands,
    code, flags, hashes, URLs, long numbers, or lists, and one sentence for questions.
  - `split_spoken_block` runs whenever an `AgentOutput` message is converted
    (`From<AgentOutput> for AIAgentText`, which reruns on every streamed append). The summary goes
    to `AIAgentText::spoken` and is kept out of `sections`, so the screen shows only the full
    answer. While the tag streams (split across chunks, including a partial `</spo`), the summary
    so far is spoken and nothing is shown.
  - `spoken_reply_text` speaks only `spoken` when present. Otherwise `fallback_readout` drops code
    blocks, inline code, paths, URLs, hashes, flags, long numbers, tables, lists, and markdown, and
    keeps the first two sentences (40 words at most); a trailing partial word is held back while
    streaming. Code-block summaries and command-result readouts are not spoken; approval prompts are.
- **Stop and mute**:
  - Esc in the input stops the readout when a reply is being read (otherwise Esc behaves as
    before). A "Stop speaking" palette entry (bound to Esc at the workspace level, active only
    while speaking) does the same, as does the "■ Stop" button next to the window-header mic
    indicator.
  - Stopping (`ConversationController::stop_speaking`) drops the rest of the reply being read and
    does not resume it; unlike "stop"/barge-in, the next reply of the turn is spoken normally.
  - Mute (header toggle and "Mute/Unmute voice conversation readout" palette entries) keeps
    conversation mode on and silences everything, including approval prompts (which then wait for
    the screen), until unmuted. The header shows "(muted)".
- **Microphone recovery** (`voice_input::recovery`): the cpal input stream lives on a supervisor
  thread. Underruns/overruns (`Xrun`), real-time denial, and route changes are transient: logged
  once and the audio is dropped. Fatal errors, or no audio for 2 s, rebuild the stream with backoff
  (250 ms doubling, 5 tries in a row; audio resets the count). Only when recovery gives up does
  hands-free stop with a toast.
- **Echo cancellation (macOS voice-processing unit)**: `voice_input::voice_processing`, compiled on
  macOS with the `voice_input/voice_processing` feature (enabled by the app's `voice_input`
  feature), behind the setting `agents.voice.conversation_echo_cancellation` (off by default).
  - One `kAudioUnitSubType_VoiceProcessingIO` unit (coreaudio-rs) owns the default microphone (bus
    1, client format 16 kHz mono f32) and speaker (bus 0, 24 kHz mono f32; the unit converts to the
    hardware rates, and any other rate it reports is resampled with `LinearResampler`). Other-audio
    ducking is set to the minimum where `kAUVoiceIOProperty_OtherAudioDuckingConfiguration` exists
    (macOS 14+). The unit also gives noise suppression and enables the macOS Mic Modes menu (Voice
    Isolation etc.).
  - The unit is a process-wide engine that runs while any lease is held: `CaptureLease` (from
    `LiveCapture`) installs the microphone sink; `OutputLease` (from `SpeechOutput`) feeds speech
    through `PlaybackQueue`, a fixed 2^20-sample lock-free SPSC `SampleRing` (flushed for barge-in)
    plus a try-locked end-of-clip slot. Callbacks never block or take the engine lock; no threads
    are added beyond the Core Audio I/O thread and the existing capture supervisor.
  - Backend choice: `set_voice_processing_wanted` (on at conversation start when the setting is on,
    off at stop) sets the `BackendSelector`; both `LiveCapture` and `SpeechOutput::open` go through
    `open_with_fallback`, which uses cpal when the unit isn't wanted, unsupported, or fails to
    start, and skips the unit after 2 failed starts until it is wanted again. Changing the
    preference sends `Signal::Reopen` to every capture supervisor, which rebuilds the microphone
    on the new backend without spending recovery retries, so hands-free keeps its socket.
  - Device changes: a Core Audio listener on the default input/output device sets a restart flag
    and reopens captures; the unit is rebuilt on its next use (capture reopen or `play`). If the
    unit stops and can't restart, `SpeechOutput::play` moves to a cpal stream so clips still
    finish, and the capture stalls and is reopened on cpal by the supervisor.
  - `voice_input::is_echo_cancelling()` is true only while the unit runs with both a capture and an
    output attached. The conversation refreshes `set_echo_cancelled` from it on speech start, on
    each clip played and on each final, so loudspeaker barge-in takes 200 ms and overlapping finals
    are kept; the TTS-text similarity filter stays as a safety net.
  - Tests: ring buffer (order, padding, capacity, wraparound, flush, cross-thread), playback end
    and stop semantics, selector and fallback state machine with fake openers, reopen
    registration, and supervisor reopen without retries. The Core Audio code itself is only
    compile-checked.
  - Manual verification (MacBook speakers, no headphones): set
    `agents.voice.conversation_echo_cancellation = true`; start voice conversation; the log shows
    "using the echo-cancelling voice-processing unit" for the microphone and speech output. Ask
    for a long answer and say "wait, stop" over it: playback stops within ~0.3 s and the utterance
    becomes the next prompt. Let a reply play without speaking: nothing of it is transcribed. Check
    Control Center shows Mic Mode. Switch output to headphones and back mid-conversation: speech
    continues and the log shows the unit restarting. Turn the setting off: logs show cpal and the
    600 ms half-duplex behaviour returns. Check other apps' audio is only lightly ducked.
  - Unverified on hardware in this change: real echo suppression quality, VPIO behaviour with
    Bluetooth headsets (it can switch them to the HFP profile), aggregate devices, and whether the
    unit's `kAudioDevicePropertyBufferFrameSize` query in `set_input_callback` succeeds on every
    device (if not, the start fails and cpal is used).
- **System-wide echo cancellation (macOS 14.2+)**: `voice_input::voice_processing::system_aec`,
  behind `agents.voice.system_echo_cancellation` (default on, macOS only; Settings → Warp Agent →
  Voice "Cancel Sound From Other Apps" and a Command Palette enable/disable pair). It removes audio
  played by *any* process, including our own TTS, from the microphone.
  - Pipeline: a private global `CATapDescription` process tap (no exclusions) and the default input
    in one private aggregate device (tap drift compensation on, rate set to 48 kHz when possible;
    16/32/48 kHz accepted, otherwise the start fails). The aggregate's I/O proc splits each buffer
    into the microphone (first buffer, channel 0) and the tap (last buffer, channels averaged),
    accumulates 10 ms frames, and runs sonora (pure-Rust WebRTC AEC3): `process_render_f32` with the
    tap, then `process_capture_f32` with the microphone. AEC3 finds the ~75 ms delay itself. Output
    is resampled to 16 kHz with rubato `FftFixedInOut` and pushed into a lock-free `SampleRing`. The
    I/O proc never locks, allocates (buffers are preallocated) or makes property calls. A worker
    thread drains the ring every 10 ms into the `LiveCapture` sink.
  - Processing: AEC3 always; AEC3 noise suppression only when `agents.voice.denoise` (RNNoise) is
    off (changing it rebuilds the pipeline); AGC and the other APM stages are off.
  - Selection, per microphone open: setting on + macOS ≥ 14.2 + tap not blocked + fewer than 2
    failed starts → system AEC; else the voice-processing unit (conversation mode with
    `conversation_echo_cancellation`); else cpal. Hands-free, conversation and speaker enrollment
    all open the microphone through `LiveCapture`, so voiceprints are enrolled and verified on the
    same path. `SpeechOutput::open` uses plain cpal while the pipeline runs (the tap hears it).
    `is_echo_cancelling()` is true while a microphone is attached to a live pipeline, so barge-in
    uses the short delay.
  - Lifetime: started on first use and kept warm after the last microphone detaches, for
    `IDLE_LINGER` (10 min), because startup takes 2–4.5 s; while it is warming up the worker pings
    the sink so the supervisor doesn't count the delay as a stall (no audio after 8 s fails the
    start). A default input/output device change or a stall (no frames for 1 s) rebuilds it on the
    next open; supervisors are told to reopen right away. Turning the setting off stops it.
  - Permission: creating the tap makes macOS ask for system audio recording
    (`NSAudioCaptureUsageDescription` in the plist; needs a signed bundle). Denial is silent: the
    tap delivers zeros. `TapHealthDetector` (every 500 ms after audio starts) marks the tap healthy
    once it carries sound, and blocked after 3 s of cumulative silence while some process reports
    `kAudioProcessPropertyIsRunningOutput`. Blocked → fall back (microphones reopen), log at info,
    and send one notice per run; `HandsFreeSession` shows a persistent toast "Allow neo-warp to
    record system audio in System Settings › Privacy & Security to cancel sound from other apps."
    with a link to the Privacy_AudioCapture pane. Toggling the setting off and on retries.
  - Tests: health detector and selector with fakes (`system_aec/mod_tests.rs`), output-backend
    choice. The Core Audio code is exercised by `cargo run -p voice_input --example
    system_aec_probe` (offline AEC3 on recorded tap + microphone: 30–50 dB cancellation, ~0.6% of a
    core).
- To make "stop" reliable on speakers without echo cancellation, short stop phrases (also "enough"
  and "that's enough") are honoured even when they overlap playback and never barged in; they are
  checked before the echo filter.
