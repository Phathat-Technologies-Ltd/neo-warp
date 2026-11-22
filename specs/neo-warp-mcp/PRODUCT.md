# neo-warp MCP

## Stage 1: the organization agent uses the user's MCP servers

In organization builds the agent runs in the embedded server (`crates/selfhost_server`). Before
this stage it could only use the built-in client tools (shell, read, edit, grep, find), so the MCP
servers a user configured in Settings > Agents > MCP servers (or a project `mcp.json`) were
invisible to it.

### Behavior

- Every MCP server that is running and in scope for the session's working directory, and every
  tool it lists, is offered to the model on each turn. Turning a server off, or a server failing,
  removes its tools from the next turn.
- The model sees each tool under a name that carries its server, `mcp__<server>__<tool>`, so two
  servers can expose tools with the same name. The tool's description is prefixed with the server
  name and keeps the server's own description and input schema.
- When the model calls an MCP tool, the call appears in the conversation as a normal Warp MCP tool
  call and the client runs it on the right server. Approval follows the user's execution profile
  exactly as for any MCP call (the MCP allowlist/denylist and "ask" setting); the server never
  bypasses it.
- The tool's result (text, embedded text resources) goes back to the model. Images and binary
  resources are described rather than inlined. A failed or cancelled call is reported to the
  model as an error so it can recover or explain.
- Very large results are cut to the first and last 50 KB with a note of how much was omitted.
- If the model calls an MCP tool that is no longer available (for example the server was turned
  off mid-conversation), the call is not sent to the client.
- Clients that don't advertise MCP tool support get no MCP tools.

### Out of scope for stage 1

MCP resources (`ReadMcpResource`), per-session server selection, and tool classification
(read-only / idempotent / destructive / external).

## Stage 1 follow-ups

- A tool's model-facing name no longer depends on the order servers are listed in. When two
  tools would share a name, the later one gets a suffix derived from its server id and tool name,
  so adding, removing, or reordering servers between turns doesn't rename it.
- A server whose name has no letters or digits (empty, or only non-ASCII) is named after its
  server id instead, e.g. `mcp__<server id>__<tool>`.

## Stage 2: choosing MCP servers per session

### Behavior

- The agent view footer has an **MCP Servers** button (part of the default right-hand toolbar,
  and in the toolbar editor for users with a custom layout). Clicking it opens a menu listing
  every MCP server the session could use: running installed servers, file-based servers in scope
  for the session's working directory, CLI-spawned and built-in servers, sorted by name. Enabled
  servers show a check mark. Clicking a server toggles it, and the menu stays open so several can
  be toggled in a row. With no running servers the menu says "No MCP servers running".
- Only enabled servers' tools are sent with the session's agent requests, so the agent can't see
  or call a disabled server. Toggling takes effect on the next request in that pane, including the
  next turn of an ongoing conversation.
- Choices belong to the terminal pane, so they carry across the conversations started in it and
  don't affect other panes. They last for the app session; a restored or new pane starts from the
  default.
- The default is the setting **Enable all MCP servers in new sessions** (Settings > Agents >
  Input; Command Palette: enable/disable "all MCP servers in new agent sessions"; on by default).
  When it is off, every server starts disabled in a pane until the user turns it on there. A
  server the user hasn't toggled in a pane follows the current value of the setting.
- Session viewers of a shared session don't get the button; the selection is the host's.
- The headless TUI has no picker yet; its requests follow the setting.
- Older clients that send the flat (ungrouped) MCP tool list aren't filtered, since that list
  doesn't say which server a tool belongs to.
