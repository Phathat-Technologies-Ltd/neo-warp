# neo-warp agent guide

neo-warp is the organization build of this fork: the unchanged Warp client talks to an embedded
server (`crates/selfhost_server`) with no Warp backend. These rules add to the root `AGENTS.md`.
Feature specs live in `specs/neo-warp-*` and `specs/self-hosted-sso/`.

## Builds and releases

The macOS app is built with `script/neo-warp/package-app <version>`.

- Sign every build, including daily development builds, with the Developer ID Application
  identity: set `NEO_WARP_SIGNING_IDENTITY` to that identity's name from
  `security find-identity -v -p codesigning`. Keychain items and macOS privacy grants (microphone,
  system audio recording) are tied to the app's signature, so mixing Apple Development and
  Developer ID builds on one Mac causes repeated keychain prompts and lost permissions.
- Notarize only builds shared with other people: also set `NEO_WARP_NOTARY_PROFILE` to the
  `xcrun notarytool store-credentials` profile. Notarization adds several minutes; daily builds
  skip it.
- Distribute a signed, notarized DMG, not a zip: `script/neo-warp/make-dmg <app> <output.dmg>`
  builds a drag-to-Applications image and notarizes and staples it. Keep shared releases outside
  `target/`, because every `package-app` run overwrites `target/neo-warp/`.
- Don't edit a script or any other build input while a build is running: bash reads scripts as it
  runs, so a mid-run edit breaks the build.
- After adding icons or other files under `app/assets/`, touch `crates/warp_assets/src/lib.rs`
  before a release build. Assets are embedded when that crate compiles, so an unchanged crate ships
  the old asset set.
- Never commit signing identities, team IDs, notary credentials, or access tokens.

## Working in parallel

- Each agent works in its own git worktree (`git worktree add .claude/worktrees/<name> -b <branch>`).
  Never switch branches in the main checkout; it may be building.
- Worktrees may share one Cargo target directory. If a build fails with type errors that don't
  match the source (artifacts from another worktree), touch the affected crate's `lib.rs`.
- Run `rustfmt --edition 2024` on the files you touched only, and revert reformatting of unrelated
  files.

## Embedded server

- The server must handle every input type the Warp client can send. Match request inputs
  exhaustively; for anything unsupported, log at warn and end the turn with a visible error rather
  than dropping it at debug level. Silently ignored inputs look like an agent that never answers.
- When the client depends on behavior that only Warp's server provided (subagent tasks, tool-call
  ordering, resume semantics), reproduce it on the server and cover it with a test that replays the
  client's request sequence, rather than changing client code.

## Voice

- When adding a voice or settings singleton the app reads at startup, register it in the test
  setups (`register_voice_models_for_tests` in `app/src/test_util`) so view tests don't panic.
- Evaluate voice changes automatically: synthesized audio (`say`, Kokoro), public datasets, and
  measured metrics (WER, precision/recall, false alarms, latency, CPU, memory). Don't rely on the
  user listening.
- Log voice decisions (speaker checks, barge-in, echo, end of turn) at info, but never log
  transcript text above debug.
- Never read the user's own recordings or data from other apps without asking.
- Local models are downloaded on demand with user consent, pinned by commit SHA, size, and SHA-256
  in `crates/local_transcription/src/model.rs`. Only small models (under ~20 MB) are embedded in
  the binary. Publishing a model or any file publicly (Hugging Face or elsewhere) needs the user's
  explicit approval.
