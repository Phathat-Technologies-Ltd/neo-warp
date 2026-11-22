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
