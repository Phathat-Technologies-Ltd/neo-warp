# Local Voice Input for neo-warp: Product Spec

## Summary

Voice input in neo-warp is transcribed on the user's own computer by NVIDIA Parakeet TDT 0.6B v3,
instead of by Warp's server and Wispr Flow. The model is not bundled: it downloads on demand, and
only after the user agrees. Recording, the microphone button, the hold-to-talk key, and the text
that lands in the input all behave as they do in Warp.

## Problem

neo-warp runs without Warp's backend, so voice input has nowhere to send audio. Sending it to a
third-party speech service would also leave the user's machine, which organizations deploying
neo-warp often don't want.

## Goals

- Voice input works in organization builds with the embedded server, with no Warp account, no
  Warp voice quota, and no audio leaving the machine.
- Nothing large downloads without the user's say-so. The user is asked the first time they start
  recording, and can download or remove the model from Settings at any time.
- The download shows progress, survives interruption (it resumes), and is checked against a
  pinned checksum before use.
- The speech engine is swappable later without changing the client.

## Non-goals

- Streaming (live) transcription while the user talks. Audio is transcribed when recording stops,
  as in Warp.
- Bundling the model, or picking among several models in the UI.
- Voice input for the headless terminal app (`warp_tui`); it has its own voice setting and is out
  of scope here.
- GPU acceleration. The int8 model runs on CPU.

## User experience

### First recording

1. The user presses the microphone button or holds the voice key.
2. If the model isn't downloaded, recording doesn't start. A toast says: "Voice input is
   transcribed on this computer by Parakeet TDT 0.6B v3. Download the model (671 MB)? Nothing is
   downloaded until you choose to." with a **Download** link.
3. Clicking **Download** starts the download and closes the toast. Dismissing the toast downloads nothing; the next
   recording attempt asks again.
4. While the model downloads, a recording attempt shows "The speech model is still downloading
   (NN%)." and doesn't record.
5. Once downloaded, recording works with no further prompts, including after restarts.

### Settings

Settings > AI > Voice, below Speech Language, shows a **Speech Model** row whenever voice input is
enabled and transcribed locally:

| State | Description | Button |
| --- | --- | --- |
| Not downloaded | "Parakeet TDT 0.6B v3 runs on this computer. Not downloaded (671 MB)." | Download |
| Downloading | "Downloading Parakeet TDT 0.6B v3 (NN%)." | Cancel |
| Downloaded | "Parakeet TDT 0.6B v3 is downloaded (671 MB)." | Remove |
| Failed | "The download failed: <reason>" | Retry |

The voice description reads "Speech is transcribed on this computer." instead of naming Wispr
Flow. Searching settings for "model", "download", or "parakeet" finds the section.

Cancel keeps what was downloaded so far; the next download resumes from there. Remove deletes the
model and any partial download.

### Errors

- Network failures and checksum mismatches show in the Settings row as "The download failed:
  ...". A file that fails its checksum is deleted so the retry starts clean.
- If the model files disappear from disk, the next recording asks to download again.

## Gating

Local transcription only applies when the embedded server is running (organization config
without `server_url`) and the organization's `voice_input` flag is on. When an organization turns
`voice_input` off, voice input disappears entirely: no mic button, no Voice section in Settings
(so no Speech Model row), no download prompt, no voice keybinding or palette entry, and the app
never calls `/ai/transcribe` or downloads the model. With a remote server, the Speech Model row
and the download prompt never appear. Voice input itself also stays under the existing voice
setting and `voice_input` build feature.

## Languages

Parakeet TDT 0.6B v3 transcribes 25 European languages and detects the language itself; the
Speech Language setting is passed along but the model doesn't need it.
