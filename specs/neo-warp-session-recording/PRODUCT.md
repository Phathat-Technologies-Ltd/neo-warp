# Session Recording

## Summary

Let users record a terminal pane to a local file and later list, play, and share their recordings.
Inspired by [asciinema](https://github.com/asciinema/asciinema). Recordings use the asciicast v2
format, so they also work with the asciinema CLI (`asciinema play`), its web player, and `agg`.

## Goals

- Start and stop recording any local terminal pane in one step.
- Recordings are saved locally, with no upload, and are readable by standard asciinema tools.
- Find, play, rename, delete, reveal, and copy recordings inside the app.
- Keep secrets out of recordings by default.

## Non-goals (v1)

- Uploading to asciinema.org or any server.
- Recording remote (SSH-extension), shared-session viewer, or cloud-agent panes.
- GIF/MP4 export (see Follow-ups).
- Recording Warp UI around the terminal (blocks chrome, agent views, input editor).

## What is recorded

- The pane's terminal output: the bytes the shell and the programs in it write to the PTY
  (`"o"` events), and terminal size changes (`"r"` events).
- Agent replies are not part of the recording. Warp renders agent conversations itself, outside
  the PTY stream, so only the terminal (PTY) output appears. Commands an agent runs in the
  terminal do appear, because their output goes through the PTY.
- Keyboard input (`"i"` events) only when the user turns on **Record keyboard input**. Off by
  default. Even when on, input is never recorded while a password prompt is showing (the terminal
  has echo turned off, as for `sudo`, `ssh`, `passwd`), up to the end of that line.

## Redaction

Before anything is written, output and input are scanned for secrets, and each match is replaced
with `[REDACTED]`. The built-in set covers:

- API keys like `sk-…` and `csk-…`
- AWS access key IDs (`AKIA…`, `ASIA…`) and secret keys in `key=value` form
- GitHub tokens (`ghp_…`, `gho_…`, `github_pat_…`) and Slack tokens (`xox…`)
- JWTs
- `Authorization:` headers and `Bearer` tokens
- Passwords in URLs (`https://user:pass@host`)
- `-----BEGIN … PRIVATE KEY-----` blocks
- `key=value` / `key: value` pairs where the key contains password, passwd, secret, token,
  api_key, or access_key

Secrets split across terminal reads are still caught. To do that the recorder holds back the
last unfinished word of output until the next whitespace arrives, so in playback a word can
appear slightly later than it did live.

Admins can add patterns in `organization.toml`:

```toml
[recording]
redact_patterns = ["corp-[0-9]{6}", "internal_[a-z]+_key"]
```

Users can add patterns in the settings file under `session_recording.redact_patterns`, one
regular expression per line. Invalid patterns are logged and skipped. Redaction is best effort: it
cannot recognize every secret, and users should still review a recording before sharing it.

## User experience

### Starting and stopping

- Pane context menu and pane header overflow menu: **Start recording** / **Stop recording**.
- Command palette: **Start or stop recording this pane**.
- Slash command `/record` toggles recording for the current pane.
- While recording, the pane header shows a red **● REC** indicator. Clicking it stops recording.
  (The pane header is shown while recording even when the pane is not split.)
- Starting shows a toast with the file name and **Show in folder**. Stopping shows "Recording
  saved" with the same link.
- Closing the pane stops and saves the recording. If the app quits or crashes, everything up to
  the last idle moment is already on disk (the writer flushes whenever the terminal is idle).

### Storage

- Default: `<app data dir>/recordings/<YYYY-MM-DD_HH-MM-SS>-<pane title>.cast`. A number is added
  if the name is taken.
- **Save recordings to** on the Recordings settings page changes the directory (`~` works). Empty
  means the default.
- The file header records the terminal width and height, start time, title (pane title and
  working directory), and `SHELL` and `TERM`.

### Recordings page (Settings > Recordings)

- Lists recordings in the directory, newest first: title, date, duration, size.
- Per recording: **Play**, **Reveal in Finder** (Show in folder on other platforms), **Copy**,
  **Rename**, **Delete** (asks for a second click to confirm).
- **Copy** puts the file's contents on the clipboard (and the file path, where the platform takes
  file paths), ready to paste or upload.
- **Play** opens a read-only player at the top of the page: the terminal screen, Play/Pause,
  Restart, −5s/+5s, speed (0.5x, 1x, 1.5x, 2x, 4x), a seek slider, and elapsed / total time.
- Also on this page: the directory setting and the **Keyboard input** toggle.
- Command palette: **Open Settings: Recordings**.

### Feature flag

The org product-area flag `session_recording` (default on) controls the feature. When off, every
entry point above is hidden: menu items, the palette entries, `/record`, the indicator, and the
Recordings page.

## Follow-ups

- GIF export through asciinema's `agg` when it is installed (`agg in.cast out.gif`), and a
  bundled renderer later.
- Playing in a full pane with Warp's own terminal renderer, colors, and copy.
- Recording remote and shared-session panes.
- A settings UI for redaction patterns.

## Open questions

- Should recordings be allowed while sharing a session, and should viewers be told?
- Should admins be able to force-disable input recording or force a directory?
- Retention: should old recordings be cleaned up automatically?
