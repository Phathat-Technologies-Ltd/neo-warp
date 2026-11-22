# neo-warp MCP: technical notes

## Stage 1: bridging client MCP tools into the embedded agent

### Protocol (no client change)

Upstream Warp already has everything needed in `warp_multi_agent_api`:

- `Request.mcp_context` (`MCPContext`): the client fills it in `app/src/ai/agent/api.rs` from the
  active templatable, file-based (cwd-scoped), CLI-spawned, and built-in servers. With
  `FeatureFlag::MCPGroupedServerContext` it sends `servers[]` (`id` = installation UUID, `name`,
  `tools[]` with `input_schema` as a `google.protobuf.Struct`, optional `identity`); without it,
  the deprecated flat `tools[]`.
- `Settings.supported_tools` contains `ToolType::CallMcpTool` (`app/src/ai/agent/api/impl.rs`).
- `message.tool_call.CallMCPTool { name, args: Struct, server_id, server_identity }` is executed
  by the client (`crates/ai/src/agent/action/convert.rs` ->
  `app/src/ai/blocklist/action_model/execute/call_mcp_tool.rs`), which resolves the server by
  `server_id` (when grouped context is on) or by tool name, and applies the execution profile's
  MCP permissions before running it.
- The result returns as `request.input.tool_call_result.call_mcp_tool` (`CallMCPToolResult`:
  `success.results[]` of text/image/resource, or `error.message`).

### Server changes (`crates/selfhost_server/src/agent`)

- `mcp.rs` (new): `McpTools::from_request(&api::Request)` builds the per-turn catalog from
  `mcp_context` when `CallMcpTool` is supported. Each entry maps a model-facing name to
  `(server_id, server_name, identity, tool_name)`. Names are `mcp__<server>__<tool>` with every
  character outside `[A-Za-z0-9_-]` replaced by `_`, cut to 64 characters (the Anthropic/OpenAI
  tool-name limit); collisions get a `_2`, `_3`... suffix. Flat (deprecated) tools are named
  `mcp__<tool>` with an empty `server_id`. `definitions()` yields `rmcp::model::Tool`s with the
  schema converted from `prost_types::Struct` to JSON (a missing schema becomes
  `{"type":"object"}`). `to_warp_tool(name, args)` produces `call::Tool::CallMcpTool` with the
  original tool name, args converted JSON -> `Struct`, `server_id`, and `server_identity`
  (with `display_name` filled from the server name, as warp-server does).
- `tools::result_text` renders `CallMcpTool` results: text parts joined by blank lines; text
  resources inlined under their URI; images and blob resources become a one-line placeholder;
  `Error` is a failure. Truncation reuses the existing 100 KB head/tail cut.
- `turn.rs`: the turn builds `McpTools` once, appends its definitions to the built-in ones, and
  stores it on `Stream` so `forward` resolves model tool calls through built-ins first, then MCP.
  An unresolvable call is logged and dropped, as before; `answer_open_tool_calls` answers it on
  the next query.
- Tool names are deterministic for the same server set, so tool requests saved in `server_data`
  still match their results on the next request.

### Tests

`mcp_tests.rs` (naming, collisions, schema/args conversion, result rendering) and a
`turn_tests.rs` round trip with the scripted provider: an MCP tool is offered, the model's call
becomes a `CallMcpTool` with the right `server_id` and args, and the client's result reaches the
model.
