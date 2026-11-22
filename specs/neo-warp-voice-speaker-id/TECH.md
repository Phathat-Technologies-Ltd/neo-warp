# Recognize my voice (speaker verification) — Tech

## Model

**CAM++ (3D-Speaker), English VoxCeleb, 16 kHz**, as exported to ONNX by sherpa-onnx.

| | |
|---|---|
| URL | `https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx` (the release tag really is spelled `recongition`) |
| Size | 29,596,979 bytes |
| SHA-256 | `98daf9bcf1c8abf3db374fec4c27a5eda7c58705bb3f180d057b085a72a88bb5`. Matches the digest GitHub's release API reports for the asset, and verified locally. |
| Source | ModelScope `iic/speech_campplus_sv_en_voxceleb_16k` (recorded in the ONNX metadata) |
| License | Apache-2.0. The 3D-Speaker repository LICENSE was fetched and checked. The ModelScope model card also says Apache-2.0, but it couldn't be fetched from here (blocked network), so check it again before release. sherpa-onnx is Apache-2.0. |
| Input | `x: float32 [N, T, 80]`: Kaldi fbank, 80 bins, 25 ms / 10 ms, global-mean CMN (`feature_normalize_type=global-mean` in the metadata) |
| Output | `embedding: float32 [N, 512]` |
| Params | 7.24 M (fp32) |

The release asset isn't immutable, so the SHA-256 in `campplus_en_voxceleb()` is the pin. The
`ModelStore` deletes a file that fails the checksum.

### Size and RAM: requirement not met, and why

The target was a download of about 10–15 MB and under 50 MB of RAM. No published English speaker
model meets it.
All the sherpa-onnx speaker models are fp32 with 6–7 M parameters:

| Model | Download | Params | Peak RSS growth* | Latency* (3 s) | VoxCeleb1-O EER (published) |
|---|---|---|---|---|---|
| **CAM++ 3D-Speaker (chosen)** | 29.6 MB | 7.2 M | 98 MB | 44 ms | ~0.7% |
| WeSpeaker ResNet34 | 26.5 MB | 6.6 M | 79 MB | 157 ms | ~0.9% |
| NeMo SpeakerNet | 23.4 MB | 5.8 M | — | — | ~2% (needs different 64-bin features) |
| CAM++, int8 we quantized ourselves (QUInt8 dynamic) | 8.9 MB | — | 72 MB | 26 ms | not measured |

\*Measured with Python onnxruntime on this Mac with 1 thread, no arena, no memory pattern, no
prepacking. The peak includes the transient during load.

CAM++ is about 3.5x faster than ResNet34 and more accurate. Its larger resident size is reduced by
lazy loading and idle unloading (below). An int8 export would cut the download to about 9 MB, but
nobody publishes one, so shipping it means hosting our own artifact. That is a follow-up: quantize
with ORT dynamic QUInt8, measure EER, host it, and pin it. The signed int8 QInt8 export doesn't
run, because the CPU provider has no `ConvInteger` kernel for int8 weights.

### Measured in the app's Rust path (Apple Silicon Mac, release build)

From `speaker_onnx::tests::benchmark_real_model` and `denoise::tests::benchmark_cpu`:

- Load: 180–440 ms (first use only).
- Embedding a 2.6–3.9 s utterance (clipped to the last 3 s): **13–26 ms**, plus 44 ms for the
  first run. One intra-op thread.
- Process memory with the model loaded, after the runs: **83 MB** `phys_footprint`, against
  **15 MB** for the same test binary with ONNX Runtime and a tiny model. So the CAM++ session
  costs about **68 MB** while it's loaded. Peak RSS is 118 MB. Turning off the arena, memory
  pattern, and prepacking cut load time in half. Graph optimization Level1 made memory worse.
- RNNoise: 10 s of audio denoised in 47 ms, which is **0.47% of one core**.
- Sanity check on macOS `say` voices (3 voices × 3 sentences), cosine similarity:
  same voice 0.84–0.90; different voices 0.08–0.31, except two similar female voices
  (Karen and Samantha) at 0.38–0.50. Real cross-session human scores are lower than TTS
  self-similarity, so the thresholds below are provisional. Tune them with real enrollments.

## Thresholds

`Sensitivity::threshold()` returns the cosine threshold against the voiceprint: Strict 0.62,
Normal 0.52, Loose 0.42.

## Architecture

### `crates/local_transcription`

- `fbank.rs`: a Kaldi-compatible fbank in pure Rust: Povey window, pre-emphasis 0.97, DC removal,
  `snip_edges`, 512-point radix-2 FFT, 80 mel bins from 20 Hz to Nyquist, natural log with an
  epsilon floor, and `subtract_mean` for CMN. Input scale doesn't matter after CMN, which is why
  sherpa's `normalize_samples` flag can be ignored.
- `speaker.rs`: the `SpeakerEmbedder` trait, `embedding_clip` (the last 3 s, tiled up to 1 s),
  `cosine_similarity`, `l2_normalize`, `Sensitivity`, and `Voiceprint::enroll`, which takes the
  mean of the L2-normalized enrollment embeddings and renormalizes it. `Voiceprint` records the
  model id, so a model change invalidates old voiceprints (`is_enrolled` checks it).
- `speaker_onnx.rs` (feature `speaker_id`): `OnnxSpeakerEmbedder`. ONNX Runtime CPU, 1 intra-op and
  1 inter-op thread, arena, memory pattern, and prepacking off. It shares the process ORT
  environment.
- `denoise.rs` (feature `denoise`, crate `nnnoiseless` 0.5.2, BSD-3-Clause, pure Rust):
  `Denoiser` upsamples 16 kHz to 48 kHz by linear interpolation, runs RNNoise on 480-sample frames,
  and averages groups of 3 back down. Buffers are reused, so the only allocation per chunk is its
  output vector. Latency is under 10 ms. `DenoiseStage::new(false)` passes audio through.
- `model.rs`: `campplus_en_voxceleb()` `ModelSpec`, directory `campplus-en-voxceleb-16k`.

### App (`app/src/voice`)

- `local_model.rs`: a third `VoiceModelKind`, `SpeakerIdKind`. It provides the consent toast,
  progress toast, `DownloadSpeakerModel` and `CancelSpeakerModelDownload` workspace actions, and
  `ensure_speaker_model_ready`. `is_speaker_id_enabled()` returns
  `is_hands_free_enabled() && FeatureFlag::VoiceSpeakerId`. The cancel action is now a
  `VoiceModelKind` method instead of comparing toast ids.
- `speaker_gate.rs` (pure, unit-tested):
  - `AudioTap` keeps the last 12 s of the audio actually sent to the server (after denoise),
    indexed by an absolute sample counter.
  - `SpeakerGate` records `SpeechStart` minus a 0.5 s allowance (pre-roll, VAD onset, delivery)
    and `SpeechEnd`. When a `Final` arrives, it holds the final and returns `Verify{range}`.
    Every later message queues behind the held final, so ordering is preserved. `verdict()`
    delivers the final, or delivers it with empty text on `NoMatch` so the partial is cleared, and
    then replays the queue. Empty finals, stop phrases (`speech::approval::is_stop_phrase`), and
    the paused (typing) state skip the check. `Unavailable` fails open.
- `speaker_id.rs`:
  - `EmbedWorker` is one background thread, `voice-speaker-id`, at QoS `UTILITY` on macOS. It
    loads the model on the first job and drops it after **3 minutes** idle (`recv_timeout`). The
    embedder runs once per final, never continuously, on at most 3 s of audio.
  - `SpeakerProfile` (singleton) holds the voiceprint, `is_verifying()`, `verify()`, and guided
    enrollment: `LiveCapture`, 3 phrases of 5.5 s each, silence trimmed with a 10 ms RMS gate, at
    least 1 s of speech required per phrase. The worker embeds each phrase, then
    `Voiceprint::enroll` runs and the result is saved.
  - Storage is `data_dir()/voice/voiceprint.json`, written atomically to a temp file and renamed,
    mode 0600 on Unix. We chose this over the keychain because a voiceprint is not a credential:
    it can't be replayed to authenticate anywhere. Only the owner can read the file, and it never
    syncs.
- `hands_free.rs`:
  - The capture callback runs `DenoiseStage` (from the `voice_denoise` setting), then feeds the
    tap (only when verifying), then sends to the socket.
  - Server messages go through `SpeakerGate` before the existing `handle_message`, and therefore
    before the conversation intercept. A rejected final shows the indicator for 3 s
    (`showing_rejection`, read by `mic_indicator_label`).
  - Verification on/off and sensitivity are captured when the session starts.
- Settings (`AISettings`): `voice_speaker_id_enabled` (true),
  `voice_speaker_id_sensitivity` (`"normal"`), `voice_denoise` (true), all under
  `agents.voice.*`.
- Settings UI (`warp_agent_page.rs`): the rows described in PRODUCT.md.
- Flag: `ProductArea::VoiceSpeakerId` (`voice_speaker_id`, default ENABLED in
  `default.flagd.json`) maps to `FeatureFlag::VoiceSpeakerId`, which is in `PRODUCT_AREA_FLAGS`.
  `VoiceInput` off and `VoiceHandsFree` off both disable it.

## Known gaps and follow-ups

- Barge-in (`speech_started`) still reacts to any voice activity. Speaker checks happen per final,
  so TTS echo can still interrupt playback through the existing barge-in path, though it can no
  longer insert or send text. The existing echo heuristics still apply.
- Partials from a non-matching speaker are visible until their final is checked.
- No Command Palette entries for the new toggles yet.
- Host an int8 CAM++ to meet the 10–15 MB download target, after measuring its EER.
- Tune the thresholds with real enrollments and microphones.
