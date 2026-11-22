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

## Testing
- VAD segmentation on recorded audio fixtures.
- Table tests for the spoken-form normalization.
- WebSocket protocol tests with a fake `Transcriber`.
- Editor tests for replacing the pending voice range.
- TUI render-to-lines tests for the indicators.
