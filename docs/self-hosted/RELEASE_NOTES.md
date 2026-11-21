# neo-warp release notes

## 0.2.0

- **Desktop app for macOS** (`neo-warp.app`), alongside the terminal app: the same organization
  sign-in, embedded server, goose agent, and Claude credentials, with the desktop terminal's
  blocks, tabs, and settings.
- **Continue with Claude** in the desktop settings signs in to an Anthropic Console organization
  through the bundled `ant` CLI; `/api-keys` opens it.
- Organization builds hide Warp's service features: cloud agents and handoff, session sharing,
  Warp Drive sync, billing and team pages, product announcements, and feedback to Warp.
- neo-warp keeps its settings, sessions, and credentials apart from Warp installations on the same
  machine. Terminal-app users sign in once more after upgrading.
- Long tool output no longer breaks a conversation: requests can exceed 2 MB, and a single tool
  result sent to Claude is capped at 100 KB.

## 0.1.0

The first release: a terminal coding agent that runs with no Warp service in between.

- **Organization sign-in** with any OpenID Connect provider (Microsoft Entra ID tested), configured
  by an admin-deployed `organization.toml`. Sessions survive restarts and end after 12 hours or
  when the identity provider stops accepting the user.
- **Embedded server**: sign-in, the model list, and the agent run inside the app on
  `127.0.0.1`. An optional standalone `selfhost-server` serves the same API centrally.
- **Agent**: goose's agent loop with Claude, using the terminal's tools (shell commands, read,
  edit, write, grep, find files), with the terminal's approval prompts for commands that change
  things.
- **Claude credentials**: Console login through Anthropic's bundled `ant` CLI, or a pasted API
  key. The Claude Code subscription login is included but not yet verified end to end. Errors
  show Anthropic's reason.
- **No Warp in the middle**: verified by recording every outbound connection during startup,
  sign-in, and idle use; only the identity provider and the local server were contacted, and the
  agent adds only `api.anthropic.com`. Warp's self-updater is off in organization builds.

See [README.md](README.md) for installation, configuration, and current limits.
