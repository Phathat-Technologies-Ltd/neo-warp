# neo-warp

neo-warp is a terminal with a built-in coding agent for organizations that don't want a vendor's
server between their people and their tools. It is built from Warp's open-source client and runs
without Warp's service:

- **Sign-in** goes through your own identity provider (any OpenID Connect provider; Microsoft
  Entra ID is tested). There are no Warp accounts.
- **The agent** runs inside the app. It is goose's agent loop (`oi-kit-agent`), driving Warp's
  terminal tools: shell commands, reading, editing, and searching files.
- **Models** are called directly at the provider with each user's own credential: Claude at
  Anthropic (a Claude Console login, a Claude Code subscription login, or an Anthropic API key),
  ChatGPT models with a ChatGPT plan login, and Cerebras with a Cerebras API key.

## How it connects

```
neo-warp (on the user's machine)
 ├─ sign-in ──────────────► your identity provider (e.g. login.microsoftonline.com)
 ├─ embedded server (127.0.0.1, in-process): sessions, model list, agent loop
 │    └─ model calls ─────► api.anthropic.com, chatgpt.com, api.cerebras.ai
 └─ Claude login (once) ──► Anthropic sign-in pages, through the bundled `ant` CLI
```

Nothing connects to Warp's servers. To allowlist neo-warp on a firewall, allow:

| Host | Used for |
|---|---|
| Your identity provider's issuer host (Entra: `login.microsoftonline.com`) | Sign-in |
| `api.anthropic.com` | Model calls, token refresh |
| `platform.claude.com`, `claude.ai` | The one-time Claude login in the browser |

## Install (admin)

1. Register neo-warp with your identity provider. For Entra, follow [entra-setup.md](entra-setup.md).
2. Copy `organization.toml.example` to the system-wide location and fill in your issuer URL and
   client ID:
   - macOS: `/Library/Application Support/Warp/organization.toml`
   - Linux: `/etc/warp/organization.toml`
   - Windows: `%ProgramData%\Warp\organization.toml`

   Writing there takes admin rights, which is the point: users can't point the app at another
   server or identity provider. Device management (MDM) can deploy it.
   Optionally, put a `flags.flagd.json` next to it to turn product areas on or off; see
   [feature-flags.md](feature-flags.md).
3. Distribute a release:
   - **Desktop app (macOS):** `neo-warp-<version>-macos-<arch>-app.zip` contains `neo-warp.app`;
     users move it to Applications. Build it with `script/neo-warp/package-app <version>`.
   - **Terminal app:** `neo-warp-<version>-<os>-<arch>.tar.gz`; users run `neo-warp` from the
     extracted folder, keeping the `resources/` folder next to the binary. Build it with
     `script/neo-warp/package <version>`.

   Both keep their data apart from any Warp installation (macOS: `~/.warp-oss-neo-warp`).

   Sign macOS builds (`CODESIGN_IDENTITY=... script/neo-warp/package <version>`). macOS ties
   keychain access to the app's signature, so each unsigned build asks every user for their
   password before it can read their saved sign-in and Claude login.

To revoke someone's access, remove them (or their group assignment) in your identity provider.
Their current sign-in ends within 12 hours at most, and they can't sign in again.

## Use (people in the organization)

1. Open neo-warp and choose **Sign in with Microsoft** (or your provider's name). Approve in the
   browser.
2. Type `/api-keys` and connect Claude. In the desktop app this opens Settings, where
   **Continue with Claude** signs in to an Anthropic Console organization and the Anthropic field
   takes an API key. In the terminal app, choose **Anthropic**, then:
   - **Connect with Claude: Console** for an Anthropic Console (API) organization.
   - **Connect with Claude: Claude Code** for a Claude Pro, Max, or Team subscription (not yet
     verified end to end in 0.1.0; prefer Console or an API key).
   - **Pasted API Key** for an `sk-ant-...` key your organization gave you.

   For Cerebras, paste a `csk-...` key in the **Cerebras** field (desktop) or the
   **Cerebras API key** row (terminal).

   For ChatGPT, choose **Continue with ChatGPT** under **Use your ChatGPT plan** (desktop) or the
   **ChatGPT plan (Codex sign-in)** row (terminal). neo-warp opens
   `auth.openai.com/codex/device` and shows a code to enter there.
3. Press Esc and ask the agent something. It asks before running commands that change things.

The model picker lists Claude Opus 5.5 (default), Sonnet 5.5, Haiku 5.5, and Fable 5.2. With a
Cerebras key, it also lists every model Cerebras says that key may use, fetched from
`api.cerebras.ai/v1/models` at launch and whenever the key changes. With a ChatGPT login, it
lists the models that ChatGPT account offers.

### Voice input

Voice input is transcribed on the user's computer by NVIDIA Parakeet TDT 0.6B v3; no audio leaves
the machine. The model (671 MB) isn't bundled. The first time someone records, neo-warp asks before
downloading it from Hugging Face (`huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx`); it can also
be downloaded or removed in Settings > AI > Voice. It's stored under the app's data directory in
`models/`, and each file is checked against a pinned SHA-256. Allow `huggingface.co` and its
download CDN (currently `*.cdn.hf.co`) through your proxy if you want people to use voice input.

### Session recording

People can record a terminal pane (pane menu, command palette, or `/record`) to a local asciicast
`.cast` file, and play, rename, or delete recordings in Settings > Recordings. Recordings stay on
the computer. They contain terminal output only; keyboard input is recorded only if the user turns
it on, and never at password prompts. Common secrets (API keys, tokens, passwords in `key=value`
pairs and URLs, private keys) are replaced with `[REDACTED]` before anything is written. To
redact more, add patterns to `organization.toml`:

```toml
[recording]
redact_patterns = ["corp-[0-9]{6}"]
```

Turn the feature off with the `session_recording` flag; see [feature-flags.md](feature-flags.md).

## Security model

- Sign-in tokens are validated against your identity provider's signing keys (issuer, audience,
  expiry, and a nonce tied to the sign-in attempt). The embedded server signs its own session
  tokens with a per-installation secret stored in the app's data directory (mode 0600). Sessions
  last at most 12 hours before the identity provider is asked again.
- Claude credentials stay on the user's machine (the OS keychain, and for Console logins, the
  bundled `ant` CLI's private config directory) and go only to Anthropic. Cerebras keys stay in
  the OS keychain and go only to Cerebras.
- Because the agent server runs on the user's own machine, the identity-provider check is
  enforced by the app, not by a server you control. A user who rebuilds the app could skip it,
  but would gain nothing beyond their own Claude credentials. For centrally enforced access, run
  the standalone `selfhost-server` and set `server_url`.

## Current limits (0.1.0)

- The desktop app is macOS only so far; the terminal app runs on macOS and Linux.
- In the desktop app, Claude logins use Console accounts; the Claude Code subscription login is
  terminal-only.
- The ChatGPT login is an unsupported stopgap: it uses the Codex CLI's OAuth client and ChatGPT's
  Codex backend (`chatgpt.com/backend-api/codex`), which OpenAI doesn't document for other apps,
  and could stop working without notice. OpenAI's supported route, [Sign in with ChatGPT plan
  usage](https://developers.openai.com/siwc/token-sharing-open-source), currently refuses
  Business workspaces that haven't allowed third-party apps
  (`3p_delegated_access_policy_denied`). neo-warp will move to it once OpenAI enables it.
- No cloud agents, Warp Drive, shared sessions, or other features that need Warp's service.
- Agent tools: shell commands, read/edit/write files, grep, and find files. MCP tools, plans,
  and subagents aren't wired to the agent yet.
- Sign-in needs a browser on the same machine; there's no device-code flow for SSH sessions yet.
- Over SSH, the desktop app works as a Warpified terminal, but its SSH extension (remote file
  browsing and code review) is off: it needs a Linux build of neo-warp on the remote host.

## Licenses

neo-warp is AGPL-3.0 (its UI framework is MIT), as Warp's client code is. Distributing a modified
build obliges you to offer its source to its users. It bundles goose's agent crates (Apache-2.0)
and Anthropic's `ant` CLI (Apache-2.0, licenses in `resources/bin/ant-licenses`). Third-party
Rust dependencies are listed in `resources/THIRD_PARTY_LICENSES.txt`.

"Warp" is a trademark of Denver Technologies, Inc.; the AGPL grants no trademark rights. Check the
name before distributing outside your organization.
