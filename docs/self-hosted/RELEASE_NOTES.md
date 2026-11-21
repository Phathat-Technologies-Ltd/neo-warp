# neo-warp release notes

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
