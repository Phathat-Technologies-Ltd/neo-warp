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

### Stage 2 review fixes

- The setting is now **Enable MCP servers by default in agent sessions** (Command Palette:
  enable/disable "MCP servers by default in agent sessions"; TOML
  `agents.mcp_servers.enabled_by_default`). It applies to every server the user hasn't toggled in a
  pane, including in sessions that are already open; the old name implied it only affected new
  sessions.
- Requests that aren't tied to a pane also follow the setting, so turning it off never exposes
  servers the user didn't turn on.
- The menu lists the same servers the agent request sends: both use the session's working
  directory, so file-based servers match. An open menu updates when a server starts, stops, or
  changes its tools.
- The MCP Servers button is hidden when grouped MCP context is off, since that request format
  can't be filtered per server.
- Choices still last for the app session only and are kept for closed panes (so an undone pane
  close keeps them). Persisting them with pane restoration is deferred.

## Stage 3: tool risk classes and session access levels

### Risk classes

Every tool of a running MCP server gets one of these classes, from safest to riskiest:

- **Read-only**: only reads.
- **Idempotent**: changes state, but calling it again with the same arguments has no further
  effect (set, update, put).
- **External**: reaches outside systems or the open world, or makes changes that aren't safe to
  repeat (send, post, create, deploy).
- **Destructive**: may delete or overwrite data.

A tool that can't be classified is **unclassified** and is treated as destructive everywhere.

How a class is chosen, first match wins:

1. **User**: the class the user set for that tool in Settings.
2. **Annotation**: the server's MCP tool annotations. `readOnlyHint: true` is read-only (even if
   the tool is open-world, so "read GitHub issues" stays read-only). Otherwise, following the MCP
   defaults, `destructiveHint` missing or `true` is destructive; `idempotentHint: true` with
   `openWorldHint: false` is idempotent; anything else is external.
3. **Heuristic**: words in the tool's name (split on `_`, `-`, `.`, and camelCase), then its
   description. Destructive words (delete, remove, drop, destroy, purge, truncate, kill, wipe,
   overwrite, revoke...) win over external (send, post, create, publish, deploy, push, execute,
   run, email, message, comment, merge...), then idempotent (set, update, put, upsert, enable,
   disable, rename, assign, label...), then read-only (get, list, read, search, fetch, query,
   describe, show, view, find, lookup, count, status...).
4. Otherwise unclassified.

Classes are computed whenever the server's tool list is read, so a server that adds or changes
tools is reclassified immediately. Only user choices are stored (locally, per server installation
and tool name; not synced).

### Settings

In Settings > MCP servers, a running server's expanded tool list shows each tool with its class
and where it came from, e.g. `create_issue · External (heuristic)`, or `· Unclassified`. Clicking a
tool cycles its class: Read-only, Idempotent, External, Destructive, then back to automatic.

### Session access levels

In the footer's MCP Servers menu, each enabled server has an **Access** row under it showing its
level; clicking cycles through:

- **Read-only**: read-only tools only.
- **Read + idempotent**: also idempotent tools.
- **All but destructive**: everything except destructive and unclassified tools.
- **All** (default, matching stage 2): every tool.

Only tools within the level are sent to the agent, so it can't see or call the others; a
destructive or unclassified tool is never offered below **All**. The level belongs to the pane
like the on/off choice.

### Out of scope for stage 3

Classifying with the user's selected model through the embedded server (heuristics only for
now); enforcing the level again when the client executes a tool call (the agent only learns tool
names from the request, so this is defense in depth); levels for the flat MCP format.

## Stage 3 review fixes

- **Enforced at call time.** Even if the agent names a tool it wasn't offered, Warp refuses to run
  a tool call whose server is disabled in the pane, or whose tool is above the pane's access level
  (an unclassified tool, or one the server doesn't list, counts as destructive). The MCP server is
  never contacted; the agent gets the error "tool not allowed in this session: <reason>", and the
  user isn't asked to approve a call that would be refused. The embedded agent server also refuses
  model calls to MCP tools that weren't in that turn's request.
- **Defaults apply to new sessions only.** The setting is again **Enable all MCP servers in new
  sessions** (Command Palette: enable/disable "all MCP servers in new agent sessions"; TOML
  `agents.mcp_servers.enabled_in_new_sessions`). A pane takes a snapshot of the defaults when its
  agent input is created, so changing the setting later doesn't change open panes. Requests not
  tied to a pane use the current settings.
- **Default access level.** New setting **MCP access level in new agent sessions** (Settings >
  Agents > Input; TOML `agents.mcp_servers.default_access_level_in_new_sessions`; default **All**).
  Each pane snapshots it with the enabled default, and each server's level in the pane starts from
  it until changed in the footer menu.
- **Heuristic is more conservative.** The heuristic reads the tool's name and description
  together and picks the riskiest class any word signals, so `get_branch` described as "can
  delete stale branches" is destructive.
