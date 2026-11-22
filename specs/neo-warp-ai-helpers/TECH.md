# neo-warp AI Helpers: Tech Spec

See `PRODUCT.md` for scope.

## Client contracts

| Helper | Client call | Transport | Carries model and credentials? |
| --- | --- | --- | --- |
| Prompt suggestions (MAA, `PromptSuggestionsViaMAA` on) | `PassiveSuggestionsModel::send_request` in `app/src/ai/blocklist/passive_suggestions/maa.rs` through `warp_multi_agent_client` | `POST /ai/passive-suggestions`, protobuf `warp_multi_agent_api::Request` in, SSE of base64 `ResponseEvent` out | Yes: `settings.model_config`, `settings.api_keys`, `settings.custom_model_providers` |
| Prompt suggestions (legacy, `PromptSuggestionsViaMAA` off) | `ServerApi::generate_am_query_suggestions` in `app/src/server/server_api.rs` | `POST /ai/generate_am_query_suggestions`, JSON `GenerateAMQuerySuggestionsRequest` in, JSON `GenerateAMQuerySuggestionsResponse` out | No |
| AI command search | `generate_commands_from_natural_language` in `app/src/server/server_api/ai.rs` | `POST /graphql/v2?op=GenerateCommands`, variables `{ input: { prompt }, requestContext }` | No |

Response shapes:

- **MAA:** `Init`, then `ClientActions` with a `CreateTask` whose task holds one `ToolCall`
  message with `SuggestPrompt { display_mode: PromptChip { prompt, label }, is_trigger_irrelevant }`,
  then `Finished(Done)`. `extract_suggestion_from_stream` coalesces the messages and takes the
  first `SuggestPrompt` chip. No suggestion is `Init` plus `Finished(Done)`.
- **Legacy:** `{ "id": "<uuid>", "suggestion": { "simple": { "query", "should_plan_task" } } }`
  or `"suggestion": null`.
- **GenerateCommands:** `generateCommands { __typename: "GenerateCommandsOutput", status {
  __typename: "GenerateCommandsSuccess", commands [{ command, description, parameters [{ id,
  description }] }] } }` or `status { __typename: "GenerateCommandsFailure", type:
  AI_PROVIDER_ERROR | BAD_PROMPT | OTHER | RATE_LIMITED }`. Parameter ids become workflow
  arguments, referenced as `{{id}}` in the command.

## Design

All code lives in `crates/selfhost_server/src/agent/`, next to the agent turn, and reuses its model
plumbing.

### Model plumbing (`turn.rs`)

`resolve_model`, `provider_for`, `TurnError` and the provider error classification become
`pub(super)` so helpers pick the same endpoint (Anthropic, Chat Completions custom provider, or
Responses API such as ChatGPT's Codex backend) with the same credential rules. No behavior change.

### One-shot completion (`helpers.rs`)

`complete(settings, system, user, make_provider) -> Result<String, TurnError>`:

1. `resolve_model(settings)` for the endpoint and model slug.
2. `make_provider(&endpoint)`, the same `ProviderFactory` type the turn uses, so tests inject a
   scripted provider.
3. `Provider::complete` with no tools and `ModelConfig { max_tokens: 400, .. }`.
4. Provider errors go through the turn's `classify_message_error`, so a 401 is `InvalidApiKey`,
   a 529 or overload is `Unavailable`, and anything else is `Internal`.

### Remembered model settings (`helpers.rs`, `ModelSettingsCache`)

`AppState` gains `ai_settings: ModelSettingsCache`, a `Mutex<HashMap<uid, (Instant, Settings)>>`.
`/ai/multi-agent` and `/ai/passive-suggestions` record a copy of the request settings reduced to
`model_config`, `api_keys` and `custom_model_providers`, keyed by the signed-in user's `uid`.
`latest(uid)` returns them if they are under 12 hours old. Memory only: the cache dies with the
process, and it holds nothing a request from that same user didn't send this session. On the
embedded server that is the local user's own credentials. A standalone shared server would hold
each user's credentials in memory for up to 12 hours, which matches how long it already sees them
in flight during agent turns.

Alternatives rejected:

- New GraphQL variables or headers carrying credentials. The GraphQL schema is generated and
  checked by `cynic`, and the call sites have no `AppContext` to read `ApiKeyManager`.
- Having the app push settings to the embedded server on change. It needs new client plumbing
  across the API key, model selection and custom provider models, for the same result.

### Prompt suggestions (`suggestions.rs`)

`passive_suggestions` handler: authenticate like `multi_agent`, decode the protobuf, record
settings, then spawn `suggest(request, tx, make_provider)` and return the SSE stream (the same
`event_stream` as the agent).

`suggest` builds the user message from the trigger:

- `ShellCommandCompleted`: command, exit code, the last 4,000 characters of output.
- `AgentResponseCompleted`: the last user query and agent text from the root task, each capped at
  2,000 characters.
- `FilesChanged` and `CommandRun` (deprecated) and a missing trigger: no model call, no
  suggestion.

The system prompt asks for a single JSON object, `{"prompt": "...", "label": "..."}`, or the
word `NONE`. Parsing accepts the object anywhere in the reply (models wrap JSON in fences). A
prompt longer than 300 characters or empty is treated as `NONE`. The label is capped at 60
characters.

`generate_am_query_suggestions` handler (legacy JSON): needs remembered settings. It joins
`context_messages` (last 4,000 characters), `system_context` and `exit_code` into the same user
message, and answers `Simple { query, should_plan_task: false }` or `suggestion: null`. With no
remembered settings or on any failure it answers `suggestion: null` with HTTP 200, so the client
logs nothing alarming.

### AI command search (`command_search.rs`)

`graphql` in `lib.rs` reads the request body. `op=GenerateCommands` goes to
`command_search::generate_commands(state, user, body)` instead of the static `Operation` table.

- Reads `variables.input.prompt` and `variables.requestContext.osContext` (name, version).
- Needs remembered settings, otherwise `AI_PROVIDER_ERROR`.
- The system prompt asks for a JSON array of at most three
  `{"command", "description", "parameters": [{"id", "description"}]}` with `{{id}}`
  placeholders. Parsing takes the outermost `[...]`. Commands with an empty `command` are dropped,
  and so are parameters whose `{{id}}` doesn't appear in the command.
- Error mapping: `InvalidApiKey` → `AI_PROVIDER_ERROR`, `Unavailable` → `RATE_LIMITED`,
  `Internal` → `AI_PROVIDER_ERROR`, nothing parseable → `BAD_PROMPT`, empty prompt →
  `BAD_PROMPT`.

## Feature flags

The flag plumbing comes from the neo-flags work. This change needs two `FeatureFlag` variants
in `crates/warp_features/src/lib.rs`:

- `AiPromptSuggestions` (`ai_prompt_suggestions`): when off, `is_prompt_suggestions_enabled` in
  `maa.rs` and the legacy model return false, and the Settings toggle, the chip UI and any
  command-palette entry for prompt suggestions are hidden. When on in organization builds, also
  enable `PromptSuggestionsViaMAA`, so suggestions take the path that carries credentials.
- `AiCommandSearch` (`ai_command_search`): when off, the `#` AI command search entry, its
  command-palette entry and its keybinding are hidden, and `generate_commands_from_natural_language`
  is not called.

The server doesn't check the flags. A client with a flag off never calls the endpoint.

## Testing

Unit tests in `suggestions_tests.rs`, `command_search_tests.rs` and `helpers_tests.rs` use a
scripted `Provider`, like `turn_tests.rs`:

- Shell-command trigger with a JSON reply → `CreateTask` with a `SuggestPrompt` chip. The prompt
  sent to the model includes the command, exit code and output tail.
- `NONE` reply, deprecated trigger, and missing credential → only `Init` and `Finished(Done)`.
- Command search reply wrapped in fences → parsed commands. Parameters not referenced in the
  command are dropped. Invalid JSON → `BAD_PROMPT`.
- The cache returns the latest settings per user, keeps only model and credential fields, and
  expires old entries.
- Router tests in `lib_tests.rs`: `GenerateCommands` without remembered settings deserializes
  through the client's `cynic` types as `GenerateCommandsFailure(AiProviderError)`, and the legacy
  endpoint answers `suggestion: null`.
