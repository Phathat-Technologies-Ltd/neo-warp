# Hands-free Voice Input for neo-warp: Tech Spec

See `PRODUCT.md`. This builds on `crates/local_transcription`, `app/src/voice/local_model.rs`, and
the embedded server's `/ai/transcribe` route (`crates/selfhost_server/src/transcribe.rs`).

## Model choices

### VAD: Silero VAD v5 (chosen)
- Source: `snakers4/silero-vad`, file `src/silero_vad/data/silero_vad.onnx` (about 2.2 MB), MIT
  license. The URL is pinned to a tagged release (`v5.1.2`):
  `https://github.com/snakers4/silero-vad/raw/v5.1.2/src/silero_vad/data/silero_vad.onnx`.
- Runtime: 16 kHz input in 512-sample (32 ms) frames, with a recurrent `state` tensor (2x1x128).
  Each frame takes under 1 ms on CPU, and it runs on `ort`, which we already ship.
- Rejected alternatives:
  - WebRTC VAD (`webrtc-vad` crate): tiny, but much worse with noise and keyboard sounds, and typing
    triggers many false positives.
  - TEN VAD: good latency, but its license adds extra terms.
  - An energy threshold: fails with fan or keyboard noise.
  - pyannote segmentation: too heavy for this job.

### Streaming STT: VAD-chunked Parakeet TDT 0.6B v3 (chosen)
- Reuse the Parakeet int8 model that is already installed (671 MB).
- Partials: while an utterance is open, re-decode the growing buffer every 600 ms, capped at the last
  15 s. Parakeet int8 decodes a 5 s clip in about 0.1 to 0.2 s on an Apple Silicon CPU (estimate;
  measure during implementation).
- Final: decode the whole utterance when the VAD reports its end.
- Why this choice: among models of this size, Parakeet TDT v3 has the best WER on the Open ASR
  Leaderboard. It outputs punctuation and casing, handles technical tokens better than small
  streaming transducers, and needs no second large download. Terminal utterances are short (under
  10 s), so the cost of re-decoding stays bounded.
- Rejected for v1, but kept as possible future engines behind the `Transcriber` trait:
  - sherpa-onnx streaming zipformer (20 to 70 MB): truly streaming, but noticeably worse on
    identifiers and flags, has no punctuation, and needs the sherpa-onnx C API instead of `ort`.
  - NeMo cache-aware streaming FastConformer exports: good accuracy, but there is no maintained
    ONNX export with stable cache inputs and outputs. Revisit later.
- Post-processing in `local_transcription::terminal_text` turns spoken forms into terminal syntax.
  Examples: "dash dash no fail fast" becomes `--no-fail-fast`, "tilde slash" becomes `~/`, "dot rs"
  becomes `.rs`, and "git check out" becomes `git checkout`. It runs only in hands-free and command
  contexts and is covered by table-driven unit tests.

## Audio I/O and echo
- Capture with `cpal` (already used by `crates/voice_input`) at the device rate, then resample to
  16 kHz.
- On macOS, when conversation mode may play audio, capture through the VoiceProcessingIO AudioUnit
  (`kAudioUnitSubType_VoiceProcessingIO`) using `coreaudio-sys` or `objc2-audio-toolbox`. It
  provides the system's echo cancellation and noise suppression. See
  `specs/neo-warp-voice-conversation/TECH.md`. Hands-free on its own can use plain `cpal` input.

## Streaming endpoint
A new WebSocket route on the embedded server: `GET /ai/transcribe/stream` (upgrade).
- Client to server:
  - Binary frames of 16 kHz mono `i16` little-endian PCM, 20 to 32 ms each.
  - Text frames: `{"type":"config","language":..,"vad":{"min_silence_ms":300}}`,
    `{"type":"flush"}`, and `{"type":"end"}`.
- Server to client: `{"type":"speech_start"}`,
  `{"type":"partial","utterance":n,"text":..}`, `{"type":"final","utterance":n,"text":..}`,
  `{"type":"speech_end"}`, and `{"type":"error",..}`.
- The server:
  - Runs the VAD on every frame through a `VoiceActivityDetector` trait in `local_transcription`.
    The Silero implementation sits behind the `silero` feature.
  - Owns the utterance buffer.
  - Calls the `Transcriber` in `spawn_blocking`. At most one decode runs per stream; newer partial
    requests are merged into the next decode.
- The route binds to loopback like the rest of the embedded server and uses the same auth token.
- The client runs voice-command detection and the auto-send timer. The timer restarts on every
  `speech_start`.

## Client
- `app/src/voice/hands_free.rs`: a `HandsFreeSession` model with the states Off, Listening, Hearing,
  and Transcribing. It owns the socket and the audio stream and sends events to the editor. The
  editor keeps track of a "pending voice range" in the input and replaces it with each partial and
  final text.
- TUI: the same model, with the mic state shown in the status line.
- Settings rows go under Settings > Voice. The palette toggle entry gets a matching context flag.

## Model download
- `ModelSpec::silero_vad_v5()` pins the release file's SHA-256. Compute it with `shasum -a 256`
  during implementation, and cross-check it against the Hugging Face LFS `oid` where a mirror exists.
- The download reuses `ModelStore`, the `ModelState` consent state machine, and the
  `ProgressThrottle` toasts.

## Flags
Add `FeatureFlag::VoiceHandsFree` to `crates/warp_features`, add it to the org default flags in
`app/src/auth/organization.rs`, and document it in `specs/neo-warp-flags`. It takes effect only when
`VoiceInput` is also on.

## Implementation notes (v1)
- Silero VAD v5.1.2 is pinned at 2,327,524 bytes, SHA-256
  `2623a2953f6ff3d2c1e61740c6cdb7168133479b267dfef114a4a3cc5bdd788f` (computed with
  `shasum -a 256` on the tagged GitHub file; it has no Hugging Face LFS mirror at that tag).
- ONNX Runtime allows one environment per process, so Parakeet and Silero share one
  (`local_transcription::ort_env`).
- The consent toast and the Speech Model settings row cover a `ModelBundle`: Parakeet, plus Silero
  when `VoiceHandsFree` is on. With the flag on, a user who already has Parakeet is asked once more,
  for the 2 MB VAD, before voice input starts again.
- Spoken-form normalization runs on finals only when the input that applies them is in shell mode,
  read when the edit is applied (so switching modes or inputs mid-session uses the right rule).
  Agent prompts are natural language, and normalization would strip their punctuation and casing.
- Auto-send submits agent prompts, and shell commands only when `is_low_risk_command` accepts
  them (a conservative read-only allowlist; any shell metacharacter disqualifies). Other commands
  run only after the explicit "send" or Enter (`HandsFreeEffect::AutoSend` versus `Send`, decided
  by `sends_input`), and committing one shows a toast saying so. The agent profile command
  allowlist is not reused because it includes `echo` and `find`, which can write or delete.
- Routing: when a final starts an empty input (`PendingVoiceRange::starts_input`),
  `route_utterance` picks `InputRoute::Agent` or `InputRoute::Shell` synchronously, before shell
  normalization, and the input locks to that mode (`set_input_config` also pauses the async NLD
  classifier, so it cannot flip the mode back before the send; the lock is released on submit
  like any other). The async classifier cannot be used here because its debounced result can land
  after a spoken "send". Inputs locked by a manual toggle or `!` prefix keep their mode. Send and
  auto-send call `input_enter`, the Enter path, which routes on the input mode.
- State changes reach every input, so each refreshes or clears its "Mic on" placeholder and footer
  indicator; transcript edits apply only in the focused input of the window that started the
  session, so two windows never submit the same transcript.
- The window header (tab bar) shows a red "Mic on" label whenever the microphone is open, whatever
  has focus. It also shows the conversation-mode state.
- Typing pauses hands-free: transcripts are dropped and the client stops sending audio to the
  server (the microphone stays open so resuming is instant) until the input is cleared or sent.
  On the first paused chunk the client sends `flush`, so the server closes the open utterance
  and stops decoding at once; that utterance's `final` is dropped because transcripts are paused.
- Server errors (`error` messages, a failed socket, VAD load failures) show an error toast in the
  session's window. The socket task ends as soon as either direction ends or fails.
- Privacy pause: `HandsFreeSession` follows `WindowManager`'s application stage. When Warp
  stops being the active app, the session enters `HandsFreeState::Paused`: the capture is dropped
  (the microphone closes, so the OS mic indicator turns off), the generation is bumped so the
  socket's remaining messages (including the open utterance's final) are dropped, a pending
  partial is removed from the input (`HandsFreeEffect::DiscardPartial`), and the auto-send timer
  is cancelled. Conversation mode stays on but stops speaking. When Warp becomes active again the
  microphone and a new socket reopen and the state returns to Listening. The placeholder and the
  window-header label show "Mic paused" meanwhile. `SystemStatsEvent::CpuWillSleep` stops the
  session outright. Screen lock has no dedicated hook; it is covered when locking deactivates the
  app or sleeps the system.
- Diagnostics: the chain logs at INFO (lengths and counts only, never audio or transcript text):
  microphone device, rate, channels, and format; the first audio callback and periodic callback
  counts with peak level (a WARN when a period is pure silence, which is what macOS delivers
  without microphone access); the socket URL, connect result and HTTP status, and close reason;
  server session start, VAD speech start and end, decode requests and results; client receipt of
  each message; and which input applied each edit. Inputs that skip an edit log why at DEBUG.
- `voice::hands_free::tests` runs the client socket loop against the real embedded server
  (`spawn_embedded`, signed-in token, real WebSocket) with a fake capture source, an energy VAD,
  and a fake transcriber, and checks that partial and commit edits come out of the controller.
- `LiveCapture` accepts f32, i16, u16, and i32 input devices.
- The server sends `speech_end` as soon as the VAD ends an utterance, and `final` once its decode
  finishes. The client's auto-send timer starts at the `final`.
- Capture uses a new `voice_input::LiveCapture` (cpal plus a linear resampler to 16 kHz).
- The toggle is a Command Palette entry and keybinding (`cmd-shift-H`, or `ctrl-alt-shift-H` on
  Linux and Windows), shown only when the flags and the local server are on. The listening state
  and a "Mic on" label are shown in the input's placeholder, and the footer's voice indicator turns
  on.
- Not done in v1: the footer mic button, the level meter, the TUI status-line privacy dot, the
  Settings > Voice rows (the settings `agents.voice.hands_free_auto_send_ms` and
  `agents.voice.hands_free_voice_commands` exist in the settings file), TUI wiring, and the macOS
  VoiceProcessingIO capture path (only needed with conversation mode; see the conversation spec
  for the echo gate used instead).
- Known gaps: the window-header "Mic on" label lives in the tab bar, so it is not visible when
  the tab bar is hidden; transcript edits are dropped (not queued) while no input in the session's
  window has focus; the "Mic on" placeholder only refreshes on hands-free state changes, not on
  conversation-state changes.

## Target input

`HandsFreeSession` keeps a `VoiceTarget` (window and terminal surface). An input claims it from
`EditorEvent::Focused` via `focus_target`, which emits `StateChanged`; a just-started session is
claimed by the focused input in its window. `Input::handle_hands_free_event` applies effects and
shows the placeholder only when `is_target`, and `VoiceControls` (given its host surface) shows
live state only through `visible_voice_state(is_target, ..)`.

## Testing
- VAD segmentation on recorded audio fixtures.
- Table tests for the spoken-form normalization.
- WebSocket protocol tests with a fake `Transcriber`.
- Editor tests for replacing the pending voice range.
- TUI render-to-lines tests for the indicators.
