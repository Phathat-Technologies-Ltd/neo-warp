<h1 align="center">neo-warp</h1>

<p align="center">
  <b>Warp for organizations: your own sign-in, your own models, no Warp backend.</b><br>
  <a href="https://github.com/Phathat-Technologies-Ltd/neo-warp/releases/latest">Download for macOS (Apple silicon)</a>
</p>

neo-warp is a fork of [Warp](https://github.com/warpdotdev/warp). It signs in through your organization's identity provider and runs the agent in an embedded server on your Mac, using the model providers you connect (Claude, ChatGPT, Anthropic, Cerebras, OpenAI-compatible endpoints). Nothing is sent to Warp's servers.

**Highlights**
- Organization sign-in (OIDC), with features that need Warp's cloud turned off and hidden.
- Agent with your choice of model, MCP servers per session, and steerable long-running commands.
- Local voice: dictation and hands-free conversation with on-device speech recognition (Apple Neural Engine on macOS 15+), filler-word removal, echo cancellation, and optional voice recognition.
- Session recordings, history filters, and AI next-command suggestions.

**Install**
1. Download the latest `.dmg` from [Releases](https://github.com/Phathat-Technologies-Ltd/neo-warp/releases/latest), open it, and drag **neo-warp** onto **Applications**. Builds are signed and notarized.
2. Get `organization.toml` from your administrator and place it at `/Library/Application Support/Warp/organization.toml`.
3. Launch neo-warp, sign in, and connect a model when prompted (or type `/api-keys`).

neo-warp is not affiliated with or endorsed by Warp. Warp is open source under [AGPL-3.0](LICENSE-AGPL) and [MIT](LICENSE-MIT), and neo-warp keeps those licenses. ## Building from source

```bash
./script/bootstrap                     # platform-specific setup
./script/run                           # build and run a development build
script/neo-warp/package-app <version>  # build the neo-warp macOS app
```

Signing and release steps are in [docs/neo-warp/AGENTS.md](docs/neo-warp/AGENTS.md). See [AGENTS.md](AGENTS.md) for the engineering guide (coding style, testing, platform notes). Feature specs for neo-warp live under `specs/neo-warp-*` and `specs/self-hosted-sso/`.

## Issues and contributions

File bugs and feature requests in this repository's [issues](https://github.com/Phathat-Technologies-Ltd/neo-warp/issues). Report security vulnerabilities privately through [GitHub security advisories](https://github.com/Phathat-Technologies-Ltd/neo-warp/security/advisories/new), not in public issues.

## Licensing

neo-warp keeps Warp's licenses: the UI framework (the `warpui_core` and `warpui` crates) is under the [MIT license](LICENSE-MIT), and the rest of the code is under the [AGPL v3](LICENSE-AGPL). Third-party licenses ship with the app in `THIRD_PARTY_LICENSES.txt`.

Downloadable voice models keep their own licenses, for example NVIDIA Parakeet (CC BY 4.0) and Kokoro (Apache-2.0); each model's source and license is recorded next to its download in `crates/local_transcription/src/model.rs`.

## Acknowledgements

neo-warp is based on the open source [Warp](https://github.com/warpdotdev/warp) client. The open source libraries and models it uses are listed with their licenses in `THIRD_PARTY_LICENSES.txt` (shipped with the app) and, for voice models, next to each download in `crates/local_transcription/src/model.rs`.
