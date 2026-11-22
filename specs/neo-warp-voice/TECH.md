# Local Voice Input for neo-warp: Tech Spec

See `PRODUCT.md` for behavior.

## Current client flow

The editor (`app/src/editor/view/voice.rs`) and the CLI-agent footer
(`app/src/ai/blocklist/agent_view/agent_input_footer/mod.rs`) record through
`crates/voice_input`, which resamples to 16 kHz mono and encodes a 16-bit WAV. When recording
stops, `ServerVoiceTranscriber` (`app/src/server/voice_transcriber.rs`) posts
`TranscribeRequest { provider: "wispr", audio: <base64 wav>, language }` to
`{server}/ai/transcribe` (`ServerApi::transcribe`) and reads `TranscribeResponse { text }`. Types
live in `app/src/ai/voice/transcribe/api`.

In organization builds `{server}` is the embedded `selfhost_server`, so serving that route
locally keeps the client contract unchanged. The client keeps `ServerVoiceTranscriber`.

## Design

### `crates/local_transcription` (new, no Warp dependencies)

- `Transcriber` trait: `transcribe(&Audio, language) -> Result<String, TranscribeError>` and
  `is_ready()`. Blocking and CPU-bound by contract; callers use `spawn_blocking`. Engines are
  swapped by handing the server a different `Arc<dyn Transcriber>`.
- `decode_wav`: any PCM/float WAV to mono `f32` (`hound`), channels averaged.
- `ModelSpec` / `ModelFile`: name, URL, size, SHA-256. `parakeet_tdt_0_6b_v3()` pins the int8
  ONNX export from `istupakov/parakeet-tdt-0.6b-v3-onnx` on Hugging Face:
  `encoder-model.int8.onnx` (652 MB), `decoder_joint-model.int8.onnx` (18 MB), `vocab.txt`;
  671 MB in total.
- `ModelStore`: keeps a model in `<root>/<spec id>`. `download` writes `<file>.part`, resumes
  with `Range: bytes=N-` (falls back to a full download on a 200), sends
  `Accept-Encoding: identity` so ranges address the stored bytes, checks the cancel flag between
  chunks, verifies SHA-256, renames, and finally writes a `.verified` marker. `status()` is
  `Installed` only with every file at its pinned size plus the marker, so launches never rehash
  671 MB. `remove` deletes the directory.
- `ModelState` + `ModelEvent` -> `Option<ModelAction>`: the pure consent/download state machine
  (`NotInstalled`, `AwaitingConsent`, `Downloading`, `Installed`, `Failed`). Actions
  are `AskConsent`, `StartDownload`, `CancelDownload`, `DeleteFiles`. `StartDownload` only comes
  from an explicit user event (`DownloadRequested`, `ConsentGiven`), never from a recording
  attempt.
- `ParakeetTranscriber` (feature `parakeet`): loads `parakeet_rs::ParakeetTDT` from the store on
  first use with its own ONNX Runtime environment, keeps it loaded behind a mutex, and returns
  `ModelNotInstalled` (dropping the loaded model) when the store isn't installed.

### Runtime choice

`parakeet-rs` 0.4.4 on `ort` 2.0.0-rc.15 (ONNX Runtime), CPU execution provider, with `ort`'s
`download-binaries` default. Reasons:

- It implements Parakeet TDT decoding (mel front end, encoder, TDT greedy decoder, vocab) for the
  exact ONNX export we pin, so we write no model code. It's maintained (multiple releases in
  2026) and widely used.
- `ort` ships prebuilt, statically linked ONNX Runtime for macOS arm64/x86_64, Linux x86_64/arm64,
  and Windows, so there's no system dependency and no C++ toolchain step. It built and ran on
  macOS arm64 here.
- `sherpa-onnx` bindings would also work but need its C++ library built or downloaded separately
  and give no advantage for one offline model. Candle/burn backends lack a maintained Parakeet
  TDT port for CPU int8.
- The int8 export keeps the download at 671 MB (fp32 is 2.5 GB). A 4-second clip, including
  loading the model, transcribed in about 2.4 s in an unoptimized debug build on Apple silicon.

Only the app enables `parakeet`; `selfhost_server` depends on the crate without it, so the
standalone server doesn't build ONNX Runtime.

### Server: `/ai/transcribe` (`crates/selfhost_server/src/transcribe.rs`)

- `AppState.transcriber` / `EmbeddedConfig.transcriber: Option<Arc<dyn Transcriber>>`. The
  standalone server passes `None`.
- Requires a valid session bearer token (401 otherwise), as `/ai/multi-agent` does.
- Body: Warp's `TranscribeRequest`; reads `audio` and `language`, ignores `provider` and
  provider properties. Response: `{ "text": ... }`.
- Errors: 501 when no transcriber, 400 for missing/invalid audio, 503 when the model isn't
  installed, 500 for engine failures. The client maps non-2xx to `TranscribeError::ErrorStatus`.

### App

- Gated by the product-area flag `FeatureFlag::VoiceInput` (org flag `voice_input`, see
  `specs/neo-warp-flags`). `auth::organization::init` applies the org flags before starting the
  embedded server. `AISettings::is_voice_input_enabled` and the Settings Voice category require the
  flag, which hides the mic buttons, keybindings, palette and slash command entries, and the
  Settings section, so nothing records, calls `/ai/transcribe`, or downloads the model.
- `auth::organization::start_embedded_server` passes a `ParakeetTranscriber` over
  `voice::local_model::model_store()` (`<data_dir>/models/parakeet-tdt-0.6b-v3-int8`) when the
  flag is on, and records that voice is served locally. `is_local_voice_enabled()` = flag on and
  served locally; every new UI path checks it.
- `voice::local_model::LocalVoiceModel` singleton owns the `ModelStore` and `ModelState`, runs
  actions, and emits `StateChanged`. Downloads run on a dedicated thread with a current-thread
  Tokio runtime (the store uses `tokio::fs` and `reqwest`), reporting progress over an unbounded
  channel at most once per 0.1%, consumed with `spawn_stream_local`. Each download gets a fresh
  cancel flag; updates from a cancelled download are dropped by comparing flags. Remove cancels
  the download and joins its thread off the main thread before deleting, so no `.part` survives.
- `ensure_ready_to_record` runs before `start_listening` in both recording paths. When not ready
  it shows a persistent toast (one at a time, by object id) with a **Download** link dispatching
  `WorkspaceAction::DownloadLocalVoiceModel` (which removes the toast), or a progress message.
- Local transcription skips the Warp voice quota check (`AIRequestUsageModel::can_request_voice`).
- Settings (`warp_agent_page.rs`, `VoiceWidget`): the Speech Model row and three page actions
  (download, cancel, remove); the page re-renders on `LocalVoiceModel` events.

## Testing

- `local_transcription`: WAV decoding; download, resume from a `.part`, checksum failure,
  no re-download once installed (against a local axum server honoring `Range`); state machine
  transitions including "no download without consent"; missing model error. An ignored test
  downloads the real model and transcribes a WAV (`PARAKEET_MODEL_ROOT`, `PARAKEET_TEST_WAV`,
  run with `cargo test -p local_transcription --features parakeet -- --ignored`).
- `selfhost_server`: `/ai/transcribe` with the client's exact body against a fake transcriber;
  401, 501, 503, 400 paths.
- App: formatting helpers and model location.

## Follow-ups

- CoreML / CUDA execution providers via `parakeet-rs` features.
- Unloading the model after idle time to return ~1 GB of memory.
