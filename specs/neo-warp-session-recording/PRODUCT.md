# Session Recording

## Summary

Let users record a terminal pane to a local file and later list, play, and share their recordings.
Inspired by [asciinema](https://github.com/asciinema/asciinema). Recordings use the asciicast v2
format, so they also work with the asciinema CLI (`asciinema play`), its web player, and `agg`.

## Goals

- Start and stop recording any local terminal pane in one step.
- Recordings are saved on this machine by the app's built-in server, with no upload to any outside
  service, and are readable by standard asciinema tools.
- Find, play, rename, delete, reveal, and copy recordings inside the app.
- Keep secrets out of recordings by default.

## Non-goals (v1)

- Uploading to asciinema.org or any outside service (a cloud recordings server is a follow-up).
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
- A marker (`"m"` event) at the start of each command run in the pane, labeled with the command
  text (redacted like output). Players use markers as chapters; files without markers still play.
- Warp's own shell-integration sequences are removed from the output so recordings play cleanly
  in asciinema and other players: the private DCS hooks (`ESC P $ ...`), Warp's private OSC codes
  (9277-9280), and the prompt and working-directory marks (OSC 133, OSC 7). Window titles
  (OSC 0/1/2), hyperlinks, colors, and all other standard sequences are kept.
- The header's `idle_time_limit` is set from **Idle time limit** (default 2 seconds), so asciinema
  and the in-app player shorten long pauses.

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
- Starting shows a "Recording started" toast. Stopping shows "Recording saved" with the file name
  and, when the recording is stored on this machine, **Show in folder**.
- Closing the pane stops and saves the recording. If the app quits or crashes, everything up to
  the last idle moment (at most about a fifth of a second earlier) is already on disk.
- Secrets are redacted before events leave the recording pane. If the recordings server can't be
  reached, recording continues: events are kept in memory and resent, and the terminal never
  waits. If the server is still unreachable two minutes after stopping, the stop toast reports
  that the recording couldn't be saved.

### Storage

- Default: `<app data dir>/recordings/user-<id>/<YYYY-MM-DD_HH-MM-SS>-<pane title>.cast`, where
  `user-<id>` is a per-user folder derived from the signed-in account. A number is added if the
  name is taken. Recordings saved directly in the recordings folder by earlier builds are moved
  into the user's folder the first time that user lists or starts a recording, as long as no
  other user has a folder there; otherwise they are left in place.
- **Save recordings to** on the Recordings settings page changes the directory (`~` works). Empty
  means the default.
- The file header records the terminal width and height, start time, title (pane title and
  working directory), and `SHELL` and `TERM`.

### Recordings pane

A dedicated pane, like Warp's other panes, opened from:

- the command palette: **Open Recordings**,
- the app menu bar: **View > Recordings**,
- **Open Recordings** on Settings > Recordings.

Opening it again in the same window focuses the existing pane.

- Lists recordings in the directory, newest first: title, a **Recording** badge while still in
  progress, date, duration, size.
- Per recording: **Play**, **Reveal in Finder** (Show in folder on other platforms; shown only
  for recordings stored on this machine), **Copy**, **Rename** (not while the recording is still in
  progress), **Delete** (asks for a second click to confirm).
- Recordings require being signed in; each user sees only their own.
- **Copy** puts the file's contents on the clipboard (and the file path, where the platform takes
  file paths), ready to paste or upload.
- **Play** opens the player below the list: the terminal screen in the current theme's terminal
  colors (16 ANSI colors, 256-color and true-color, bold, italic, underline, inverse), Play/Pause,
  Restart, −5s/+5s, speed (0.5x, 1x, 1.5x, 2x, 4x), **Skip idle** (on by default; caps pauses at
  the recording's idle time limit), a seek slider, and elapsed / total time.
- When the recording has command markers: a **Commands** list (click to jump) and **Previous
  command** / **Next command** buttons. Keys: `n` / `p` for next / previous command, space for
  play/pause, left / right for −5s / +5s.

### Settings > Recordings

Configuration only: **Open Recordings**, the directory, the **Keyboard input** toggle, **Idle
time limit**, and **Extra redaction patterns** (one regular expression per line). Command palette:
**Open Settings: Recordings**.

### Feature flag

The org product-area flag `session_recording` (default on) controls the feature. When off, every
entry point above is hidden: menu items (including View > Recordings), the palette entries,
`/record`, the indicator, the Recordings pane, and the Recordings settings page.

## Follow-ups

- GIF export through asciinema's `agg` when it is installed (`agg in.cast out.gif`), and a
  bundled renderer later.
- Playing with Warp's own terminal renderer (selection and copy across the grid).
- A Recordings entry in the left tools panel.
- Recording remote and shared-session panes.

## Open questions

- Should recordings be allowed while sharing a session, and should viewers be told?
- Should admins be able to force-disable input recording or force a directory?
- Retention: should old recordings be cleaned up automatically?
