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

## Stage 1 follow-ups (`crates/selfhost_server/src/agent/mcp.rs`)

- `unique_name(base, key, used)`: on a collision the suffix is `_<8 hex digits>` of FNV-1a over
  `key` (`server_id\0tool_name`, or the tool name for flat tools) plus an attempt counter, instead
  of an order-dependent `_2`. FNV-1a is used because `DefaultHasher` isn't stable across Rust
  releases. The base is still cut so name plus suffix fit in 64 ASCII bytes.
- `server_label` falls back to the sanitized server id when the sanitized name has no ASCII
  alphanumerics.
- `json_to_value` documents that numbers go through `f64` (the only protobuf `Value` number).
- Tests: order independence of collision suffixes, the 64-character cut with a suffix, the id
  fallback, that every MCP name carries the `mcp__` prefix (so it can't shadow a built-in), and the
  precision loss above 2^53.

## Stage 2: per-session MCP server selection (client)

- `app/src/ai/mcp/session_selection.rs`: `SessionMcpSelection` maps a terminal pane's
  `EntityId` to per-server (`installation_id`) overrides. `is_enabled(pane, server, default)`
  returns the override or the default. It lives on `TemplatableMCPServerManager` (already a
  singleton in every app and test harness, so no new model registration), which exposes
  `is_server_enabled_for_session` and `set_server_enabled_for_session`. Entries for closed panes
  are kept (a few bytes each) since entity ids aren't reused. The default is the new `AISettings::mcp_servers_enabled_in_new_sessions`
  (`agents.mcp_servers.enabled_in_new_sessions`, default `true`).
- `TemplatableMCPServerManager::active_servers_for_session(cwd, app)` gathers the servers that
  `RequestParams::new` used to collect inline (installed, file-based for `cwd`, CLI-spawned,
  built-in), de-duplicated by UUID and sorted by name then UUID. Sorting also makes the request's
  server order stable instead of following `HashMap` iteration order.
- `RequestParams::new` (`app/src/ai/agent/api.rs`) keeps only servers enabled for
  `terminal_view_id`; requests without a pane keep every server. The flat legacy path is
  unchanged.
- Footer (`agent_input_footer`): new `AgentToolbarItemKind::McpServers` (agent view only, hidden
  from session viewers, in `default_right` and `all_available`). It renders an `ActionButton` and,
  while open, a `Menu<AgentInputFooterAction>` anchored to it like the usage popover.
  `ToggleMcpMenu` rebuilds the items from `session_mcp_servers(pane, cwd)` (cwd from the active
  block); `SetSessionMcpServerEnabled { server, enabled }` writes the override and reopens the menu.
- Settings: `McpServersEnabledInNewSessionsWidget` on the Warp Agent page, a
  `ToggleSettingActionPair` for the Command Palette, and the
  `MCP_SERVERS_ENABLED_IN_NEW_SESSIONS_FLAG` context flag set in the workspace keymap context.
- Tests: `session_selection_tests.rs` (default, per-pane override) and footer tests for
  the menu items.

## Stage 2 review fixes

- `refresh_mcp_menu` sits above `update_usage_button`'s doc comment again.
- `TemplatableMCPServerManager::session_server_tools(pane: Option<EntityId>, cwd, app)` is the one
  place requests and the menu decide what a session sees; a `None` pane uses the default.
- The footer takes the session's cwd from `ActiveSession::current_working_directory` (set with
  `AgentInputFooter::set_active_session` from `terminal/input.rs`), the same source as
  `SessionContext::from_session` in requests. It subscribes to `TemplatableMCPServerManager` and
  rebuilds the menu while it's open.
- `AgentToolbarItemKind::McpServers` renders nothing unless `MCPGroupedServerContext` is on.
- Setting renamed `mcp_servers_enabled_by_default` (`McpServersEnabledByDefault`, flag
  `MCP_SERVERS_ENABLED_BY_DEFAULT_FLAG`); not shipped yet, so no migration.

## Stage 3: tool risk classes and access levels

- `app/src/ai/mcp/tool_risk.rs`: `McpToolRisk { ReadOnly, Idempotent, External, Destructive }`
  (ordered by risk), `McpToolRiskSource { User, Annotation, Heuristic }`,
  `McpToolClass { risk: Option<McpToolRisk>, source: Option<_> }` (`None` risk = unclassified,
  `effective_risk()` maps it to `Destructive`). `classify_tool(&rmcp::model::Tool)` applies
  annotations then the word heuristic; `classify_tool_with_overrides(server, tool, overrides)`
  applies the user override first. `McpAccessLevel { ReadOnly, ReadAndIdempotent,
  AllButDestructive, All }` has `allows(&McpToolClass)` (compares `effective_risk()` with the
  level's maximum risk), `next()`, and `label()`.
- Overrides are the private, local-only `AISettings::mcp_tool_risk_overrides:
  HashMap<String, McpToolRisk>`, keyed `"<installation uuid>/<tool name>"`
  (`tool_risk::override_key`).
- `SessionMcpSelection` stores per pane and server `{ enabled: Option<bool>, level:
  Option<McpAccessLevel> }`. `select(pane, default_enabled, servers, overrides)` is the pure
  function behind `session_server_tools`: it drops disabled servers and returns each remaining
  server's tools filtered by its level. `RequestParams::new` builds `MCPServer.tools` from it.
- Footer: `session_mcp_menu_items` adds an indented `Access: <level>` row after each enabled
  server, dispatching `SetSessionMcpAccessLevel { server, level: level.next() }`.
- Settings: `ServerCardView` takes `Vec<ServerCardTool { name, class }>`; chips are clickable
  (one `MouseStateHandle` per tool, created with the card) and dispatch `CycleToolRisk(index)`.
  The card updates its own chip and emits `ServerCardEvent::SetToolRisk(item, tool, risk)`; the
  list page writes the override (cards aren't rebuilt, so the expanded list stays open).
- Tests: `tool_risk_tests.rs` (annotation mapping incl. MCP defaults, heuristic words and
  priority, camelCase splitting, unclassified, overrides, level filtering) and
  `session_selection_tests.rs` (`select`: disabled servers dropped, `None` pane uses the default,
  per-server levels).

## Stage 3 review fixes

- Call-time enforcement: `CallMCPToolExecutor::execute` resolves the server (`server_id`, or
  `server_from_tool` for flat calls) and calls
  `TemplatableMCPServerManager::check_session_tool_call(Some(terminal_view_id), server, name, app)`
  before any schema coercion or peer lookup; on `Err(reason)` it returns
  `CallMCPToolResult::Error("tool not allowed in this session: {reason}")` synchronously.
  `should_autoexecute` returns `true` for such calls so they're refused without a permission
  prompt. The check is `SessionMcpSelection::check_tool_call`, which classifies the server's
  advertised `Tool` (or treats a missing one as unclassified, i.e. destructive) with the same
  `classify_tool_with_overrides` and `McpAccessLevel::allows` the request filter uses.
- Embedded server: `McpTools::to_warp_tool` only maps names built from this turn's
  `mcp_context`; anything else falls through to `tools::to_warp_tool`, which refuses unknown
  names, so the call is dropped and never sent to the client. Covered by
  `calls_to_tools_not_advertised_this_turn_are_refused`.
- Per-pane snapshot: `SessionMcpSelection` keeps `PaneSelection { defaults:
  Option<SessionMcpDefaults { enabled, level }>, servers }`. `start_session(pane, defaults)` sets
  the snapshot once (`get_or_insert`); `TemplatableMCPServerManager::start_session(pane, app)` is
  called from `AgentInputFooter::new`. All lookups take the current settings as
  `current_defaults`, used only when there's no pane or the pane never started.
- Setting renamed back to `mcp_servers_enabled_in_new_sessions`
  (`McpServersEnabledInNewSessions`, `MCP_SERVERS_ENABLED_IN_NEW_SESSIONS_FLAG`). New
  `mcp_default_access_level_in_new_sessions: McpAccessLevel` (synced, default `All`), shown as a
  dropdown (`McpDefaultAccessLevelWidget`) on the Warp Agent page. `McpAccessLevel` now derives
  serde, `JsonSchema`, and `SettingsValue`, and has `ALL`.
- `classify_tool` runs the word heuristic over `"{name} {description}"` instead of the name first.
- Tests: `session_selection_tests.rs` adds the snapshot, disabled-server, level, override, and
  unclassified/unadvertised call checks; `tool_risk_tests.rs` adds description-over-name cases.

## OAuth sign-in

- `crates/oauth_loopback` (shared with organization sign-in) gained
  `LoopbackListener::wait_for_callback`, which also returns the RFC 9207 `iss` parameter (rmcp
  validates it), and now checks `state` inside the connection handler so a mismatched callback
  gets the failure page and a 400 instead of the success page.
- `crates/mcp/src/oauth/loopback.rs` wraps that listener (`/callback` path, `127.0.0.1` or
  `localhost`, 300 s timeout). Dropping the receive future cancels the listener thread, so an
  abandoned flow frees the port. OAuth `error` redirects become `CallbackResult::Error`.
- `OAuthCallbackMode::Loopback { settings, fallback }` replaces the TUI-only loopback mode;
  `CustomScheme(CustomSchemeCallback)` remains. `prepare` falls back to the custom scheme when
  the bind fails, and `make_authenticated_client` switches to it for a static provider without a
  `loopback_client`. PKCE (S256) and the `state` check are unchanged in rmcp.
- `dcr_request` sends `client_name = DCR_CLIENT_NAME` (`"neo-warp"`) and the single exact
  redirect URI (rmcp's `AuthorizationRequest` carries one).
- `org_sign_in::OrganizationConfig::mcp_oauth` (`McpOAuthConfig { redirect_port,
  redirect_host }`) is read by `auth::organization::mcp_oauth_loopback_settings`, which
  `TemplatableMCPServerManager` passes into every MCP spawn.
- The desktop manager always records the `state -> server` mapping so a fallback deep link still
  routes through `handle_oauth_callback`.
- Tests: redirect construction (default and pinned host/port), bind-failure fallback, the DCR
  request, an end-to-end DCR + loopback + code exchange that asserts the registered client name,
  redirect path, and `iss` pass-through, the `iss` field in `oauth_loopback`, mismatched-state
  400, and `organization.toml` parsing of `[mcp_oauth]`.
