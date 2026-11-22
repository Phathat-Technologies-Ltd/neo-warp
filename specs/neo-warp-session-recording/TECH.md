# Session Recording: Tech Spec

See `PRODUCT.md` for scope.

## Architecture: client capture, server store

Capture and redaction run in the app; storage is a service. Like `/ai/transcribe` and codebase
indexing, the store is served by the embedded server (`crates/selfhost_server`), so it can move
to a standalone or cloud server by pointing the app at a different server URL.

- `crates/session_recording`: the asciicast format, the redactor, naming helpers, the API types
  (`CreateRecording`, `RecordingMetadata`, ...), the `RecordingStore` trait, and `LocalDirStore`.
- `crates/selfhost_server/src/recordings.rs`: the `/recordings` routes over
  `AppState::recordings: Option<Arc<dyn RecordingStore>>`. A cloud backend (object storage plus
  a metadata table) implements the same trait and is configured in place of `LocalDirStore`; the
  standalone server reads `recordings_dir` from its config.
- App: `ActiveRecording` captures and redacts, and uploads through `RecordingsClient`
  (`app/src/terminal/session_recording/client.rs`, implemented for `ServerApi` in
  `app/src/server/server_api/recordings.rs`). Secrets never leave the machine unredacted.

The embedded server's store is a process-wide `LocalDirStore` (`client::local_store()`). The app
points it at `SessionRecordingSettings::recordings_dir()` before starting a recording and before
listing (`sync_local_store_root`), so changing **Save recordings to** takes effect without a
restart.

### API

All routes need a bearer ID token from the server's sign-in; the owner is the token's user ID.
Missing or invalid tokens get 401; a server without a store gets 501.

| Route | Body | Response |
| --- | --- | --- |
| `POST /recordings` | `{header, name}` (asciicast header) | 201 `{id}` |
| `POST /recordings/{id}/events?seq=N` | asciicast event lines (NDJSON) | 204 |
| `POST /recordings/{id}/finish` | none | 200 metadata |
| `GET /recordings` | none | 200 `[metadata]`, newest first |
| `GET /recordings/{id}` | `Range: bytes=a-b` optional | 200 or 206 cast content |
| `PATCH /recordings/{id}` | `{name}` | 200 metadata (the ID may change) |
| `DELETE /recordings/{id}` | none | 204 |

Metadata is `{id, title, started_at, duration, size_bytes, local_path?}`; `local_path` is set
only when the store is on the client's machine, which is what shows **Reveal in Finder**.

Batches are numbered from 0 per recording. The store acknowledges a batch it already has without
writing it again (so resending after a lost response is safe) and rejects a gap with 409
`{expected_seq}`. Event lines are validated as `[time, code, data]` arrays. IDs are file stems;
an ID containing a path separator or starting with `.` is never resolved.

`LocalDirStore` keeps each owner in `<root>/user-<first 16 hex of sha256(owner)>/`. It holds the
next expected sequence number of in-progress recordings in memory; after a server restart the
next batch is accepted as-is. Metadata reports `in_progress` for those, and `PATCH` on one
returns 409 (`StoreError::InProgress`), since the uploader keeps appending under the old ID; the
page hides **Rename** for them.

On `create` and `list`, `LocalDirStore` moves `.cast` files in the root (the pre-per-user layout)
into the caller's folder when no other `user-*` folder exists, skipping names already taken.
With other users present it leaves them and logs. Repeating it is a no-op.

## Format

asciicast v2 (<https://docs.asciinema.org/manual/asciicast/v2/>): a JSON header line
(`version`, `width`, `height`, `timestamp`, `title`, `env`), then one JSON array per event:
`[time, code, data]`. `time` is seconds since the start, with microsecond precision. Codes used:
`"o"` output, `"i"` input, `"r"` resize (`"COLSxROWS"`). The reader also accepts `"m"` markers
and skips unknown codes. Event data must be a UTF-8 string, so PTY bytes go through an incremental
decoder that holds a character split across reads and replaces invalid bytes with U+FFFD.

Code: `crates/session_recording/src/asciicast.rs` (`CastWriter`, `Utf8Decoder`, `parse`,
`read_summary`, `validate_event_lines`).

## Capturing PTY output

The local PTY event loop already broadcasts every read on an `async_broadcast` channel
(`ChannelEventListener::pty_reads_tx` in `crates/warp_terminal/src/event_listener.rs`), created
inactive and handed to `TerminalView` as `inactive_pty_reads_rx` (used today by the debug
`PtyRecorder`). The sender only allocates when a receiver is active, so idle panes pay nothing.

`ActiveRecording` (`app/src/terminal/session_recording/recorder.rs`) activates a receiver and
runs two threads. The capture thread:

- waits on either the PTY receiver or a control channel (resize, input, password prompt, stop),
- timestamps each event with a monotonic `Instant` measured from the start,
- decodes, redacts, and encodes event lines into a batch, cut when it reaches 64 KB or when both
  channels are idle and 200 ms have passed since the last batch,
- on stop: flushes the redactors' held text into a last batch and exits.

The upload thread creates the recording on the server, then sends batches in order. A failed or
timed-out (15 s) request leaves the batch queued and retries with exponential backoff (250 ms to
10 s) while new batches keep queuing, so the capture thread never waits on the network. More than
256 MB unsent is dropped with a warning. After stop it keeps retrying for up to two minutes, then
calls finish; `stop()` returns a future of the result, which the view turns into the "saved" or
error toast. Dropping `ActiveRecording` stops capture and lets the upload finish in the
background.

This never touches `TerminalModel` or its lock: bytes come from the event loop's broadcast, and
size changes come from `TerminalView::resize_internal` after the model is already resized, using
the view's own `size_info`. Dropping `ActiveRecording` (pane closed, view dropped) stops it.
`try_broadcast` drops reads if the receiver falls 1024 reads behind; the recorder thread does
little work per read, and any drop is logged.

Recording is offered only for panes with a local PTY (`inactive_pty_reads_rx` is `Some`) and
builds with `local_fs`.

## Input and password prompts

Opt-in setting `session_recording.record_input`. Input is taken in
`TerminalView::write_user_bytes_to_pty`, which carries keystrokes sent to a running program
(Warp's own input editor sends commands through a separate path; the shell echoes those, so they
appear in output).

Input is dropped up to the next line end when either:

1. Warp's termios poller reports ECHO off with ICANON on (`on_possible_password_prompt`). Polling
   now also starts for commands while a recording with input is active.
2. The recent output ends like a password prompt (`password`, `passphrase`, `passcode`, `PIN`,
   `verification code`, ending in `:` or `?`). This catches prompts before the poller's first
   tick and in shells where polling doesn't run.

## Redaction

`crates/session_recording/src/redact.rs`. One `Redactor` per stream (output, input), built from the
built-in patterns plus org (`organization.toml` `[recording] redact_patterns`, read in
`crates/org_sign_in`) and user (`session_recording.redact_patterns`, one per line) patterns.

Streaming across chunks:

- Text not yet written is `pending`; the last 256 bytes already written are `context`.
- Each push matches every pattern on `context + pending`. Matches that end in already-written
  text are ignored.
- The writable prefix ends at the start of the trailing unfinished word (the run after the last
  whitespace), so a token split across reads is matched whole. A word over 4 KB is written
  anyway.
- A match that would straddle that point is held back whole.
- An open `-----BEGIN … PRIVATE KEY-----` block is held until its END line; past 64 KB it is
  replaced by `[REDACTED]`.
- A match whose prefix was already written (`Bearer ` then the token in the next read) replaces
  only the unwritten part.

Trade-off: the last word of output appears in the file with the timestamp of the read that
finishes it. Prompts usually end in a space, so this is rarely visible.

## Storage and listing

`crates/session_recording/src/naming.rs`: file naming (`<timestamp>-<sanitized title>.cast`, de-duplicated),
listing (`.cast` files in the directory, header plus last event time, newest first, unreadable
files skipped and logged), rename (sanitized, keeps `.cast`, refuses to overwrite), and delete,
used by `LocalDirStore`. The Recordings page lists, plays (fetching the content), copies,
renames, and deletes through `RecordingsClient`.
`session_recording/settings.rs` holds the `SessionRecordingSettings` group: `directory`,
`record_input`, `redact_patterns`. These are not synced to the cloud.

## Player

`session_recording/player.rs`: a deterministic state machine (`Paused` / `Playing` / `Ended`,
speed, position) that replays events into a `vt100::Parser` (vt100 0.16). The caller supplies
elapsed time, so tests don't sleep. Seeking backwards rebuilds the screen from the start; forwards
applies events incrementally.

The UI is the Recordings settings page (`app/src/settings_view/recordings_page.rs`), which shows
the player above the list and advances it on a ~30 fps timer while playing.

Why not replay into Warp's `TerminalModel` in a new pane type: the terminal model is wired to
blocks, shell bootstrap, sessions, and a PTY event loop. A read-only pane would need a new
`BackingView`, pane-group plumbing, and a fake manager, a much larger and riskier change. vt100 is
a small, well-tested emulator, and rendering its screen as monospace text is enough for v1. The
cost: no colors or styles in the in-app player yet. External players (asciinema) show them.
Moving to a full pane with Warp's renderer is a follow-up.

## Entry points

- `TerminalAction::ToggleCastRecording`, handled in `terminal/view/session_recording.rs`.
- Pane context menu (`pane_context_menu_items`) and pane header overflow menu.
- Command palette binding `terminal:toggle_cast_recording` and
  `workspace:show_settings_recordings_page`.
- Slash command `/record` (`SlashCommandKind::Record`, GUI only).
- Pane header indicator: `render_recording_indicator`; `should_render_header` is true while
  recording.
- `SettingsSection::Recordings`, gated by the flag.

## Feature flag

`FeatureFlag::SessionRecording`, org product area `session_recording`
(`crates/org_flags/resources/default.flagd.json`, default on), mapped in
`app/src/auth/organization.rs`, documented in `docs/self-hosted/feature-flags.md`.

## Tests

- `asciicast_tests.rs`: round trip, line shape, split UTF-8, invalid bytes, parsing asciinema
  files, bad input, summary.
- `recorder_tests.rs` (fake `RecordingsClient`): output and resize order and timing, contiguous
  in-order batches, retry after failed start and appends without losing events, giving up after
  the finish deadline, upload after drop, output redaction, input off by default, input
  recording with redaction and password suppression.
- `crates/session_recording/src/local_tests.rs`: round trip, duplicate and out-of-order batches,
  invalid events, byte ranges, rename and delete, owner isolation, path-escaping IDs, root
  changes.
- `crates/selfhost_server/src/recordings_tests.rs`: HTTP round trip (start, append, finish, list,
  get, range get, rename, delete), sign-in required, users isolated, missing store.
- `redact_tests.rs`: each built-in pattern, ordinary text untouched, secrets split across chunks
  (including one byte at a time), private-key blocks across chunks, held-word behavior, extra
  and invalid patterns.
- `player_tests.rs`: paused start, timing, speed, end and replay, seek back and forward, restart.
- `naming_tests.rs`: sanitizing, collision-free names, listing, missing directory, rename and
  delete, formatting.
- `org_sign_in` reads `[recording] redact_patterns`; `org_flags` defaults include
  `session_recording`.
