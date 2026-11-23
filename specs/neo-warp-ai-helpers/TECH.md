# neo-warp AI Helpers: Tech Spec

See `PRODUCT.md` for scope.

## Client contracts

| Helper | Client call | Transport | Carries model and credentials? |
| --- | --- | --- | --- |
| Prompt suggestions (MAA, `PromptSuggestionsViaMAA` on) | `PassiveSuggestionsModel::send_request` in `app/src/ai/blocklist/passive_suggestions/maa.rs` through `warp_multi_agent_client` | `POST /ai/passive-suggestions`, protobuf `warp_multi_agent_api::Request` in, SSE of base64 `ResponseEvent` out | Yes: `settings.model_config`, `settings.api_keys`, `settings.custom_model_providers` |
| Prompt suggestions (legacy, `PromptSuggestionsViaMAA` off) | `ServerApi::generate_am_query_suggestions` in `app/src/server/server_api.rs` | `POST /ai/generate_am_query_suggestions`, JSON `GenerateAMQuerySuggestionsRequest` in, JSON `GenerateAMQuerySuggestionsResponse` out | No |
| Next Command (AI) | `ServerApi::generate_ai_input_suggestions` from `NextCommandModel` in `app/src/ai/predict/next_command_model.rs` | `POST /ai/generate_input_suggestions`, JSON `GenerateAIInputSuggestionsRequest` in, JSON `GenerateAIInputSuggestionsResponseV2` out | No; carries an optional `model` ID |
| AI command search | `generate_commands_from_natural_language` in `app/src/server/server_api/ai.rs` | `POST /graphql/v2?op=GenerateCommands`, variables `{ input: { prompt }, requestContext }` | No |

Response shapes:

- **MAA:** `Init`, then `ClientActions` with a `CreateTask` whose task holds one `ToolCall`
  message with `SuggestPrompt { display_mode: PromptChip { prompt, label }, is_trigger_irrelevant }`,
  then `Finished(Done)`. `extract_suggestion_from_stream` coalesces the messages and takes the
  first `SuggestPrompt` chip. No suggestion is `Init` plus `Finished(Done)`.
- **Legacy:** `{ "id": "<uuid>", "suggestion": { "simple": { "query", "should_plan_task" } } }`
  or `"suggestion": null`.
- **Next Command:** `{ "commands": ["<cmd>"], "ai_queries": [], "most_likely_action": "<cmd>" }`;
  no suggestion is `commands: []` and `most_likely_action: ""`. The client ignores an empty
  `most_likely_action` and requires it to start with the request's `prefix`.
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
Recording merges: a field the request leaves unset or empty keeps its earlier value, and a request
with all three empty records nothing, so a sparse request never erases credentials a fuller one
supplied.
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
  `Internal` (transient: network, unexpected reply) → `OTHER`, nothing parseable → `BAD_PROMPT`,
  empty prompt → `BAD_PROMPT`. Both `BAD_PROMPT` cases are logged at info level.

### Next Command (`next_command.rs`)

`generate_input_suggestions` handler: authenticate like the others (401 without a valid server
token), read the JSON leniently (every field defaults; `block_context` keeps only `command`,
`exit_code`, `pwd`, `git_branch`), and answer 200 with the response above in every other case.

- Needs remembered settings; without them, no suggestion.
- Model: `settings_for_model` replaces `model_config.base` with the request's `model` when
  `resolve_model` accepts it (a Claude model with an Anthropic credential, or a custom model's
  config key whose provider has a key). Otherwise the remembered (agent) model answers.
- User message: shell/OS JSON, the recent blocks (last 6,000 characters), the finished command
  with exit code, directory and branch, history sequences (last 3,000 characters), up to five
  rejected suggestions, and the typed prefix or "nothing typed". The system prompt asks for one
  command on one line, starting with the prefix, or `NONE`.
- `sanitize_command`: drops code-fence lines, rejects more than one remaining line, strips
  wrapping backticks and a leading `$ `, rejects `NONE`, control characters, more than 300
  characters, a reply that doesn't start with the prefix or equals it, and `is_destructive`
  commands. `is_destructive` lowercases, splits on `;`, `&`, newlines and pipes, drops wrappers
  (`sudo`, `env`, `FOO=bar`, ...), and checks each invocation (recursive `rm`, `git push`
  force/delete/`+ref`/`:ref`, `reset --hard`, `clean -f`, `branch -D`, `checkout .`, `mkfs`,
  `dd of=/dev/`, `diskutil erase*`, piping into a shell, `kubectl delete`, `terraform destroy`,
  SQL `drop`, fork bombs).
- Cost controls in `NextCommandState` (an `AppState` field, memory only): answers (including
  declines, not failures) are cached for 30 seconds keyed by user, model and request (256
  entries); prefix requests wait 150 ms and give up if a newer prefix request from the same user
  arrived; the model call times out after 2.5 seconds. Failures log at info/warn without command
  text; suggestions log only at debug.

### Next Command client (`app/`)

- `AISettings::ai_next_command_suggestions_enabled_internal`
  (`agents.warp_agent.active_ai.ai_next_command_suggestions_enabled`, default false) and
  `is_ai_next_command_suggestions_enabled`, which also requires Next Command.
  `NextCommandModel` calls the server only when it's true; otherwise it returns an empty response
  that `on_next_command_suggestion_result` ignores. History-based results are unchanged.
- `AISettings::ai_next_command_model` (`agents.warp_agent.active_ai.ai_next_command_model`,
  default empty = same as agent) goes out as the request's `model`.
- `Input::generate_autosuggestion_async` takes the typed-prefix Next Command path when
  `PartialNextCommandSuggestions` is on or the AI setting is on.
- Settings: `AiNextCommandSuggestionsWidget` (toggle plus, when on, a `FilterableDropdown` built
  from `query_model_picker_choices` over `get_base_llm_choices_for_agent_mode`, keeping
  selectable models and dropping auto models and custom routers). It refreshes on
  `LLMPreferences`, `ApiKeyManager` and `AINextCommandModel` changes. The Command Palette has an
  "AI next-command suggestions" toggle (context flag `AI_Next_Command_Suggestions`).
- No workspace policy or org flag hides it: neo-warp users have no team, so
  `is_next_command_enabled` is true.

## Feature flags

The `FeatureFlag` variants `AiPromptSuggestions` and `AiCommandSearch` come from the neo-flags
work (`PRODUCT_AREA_FLAGS`, on by default, set from the organization flag file).

- `AiPromptSuggestions` (`ai_prompt_suggestions`): `AISettings::is_prompt_suggestions_enabled`
  requires it, so `maa.rs` and the legacy model send no requests and no chips appear. The Settings
  `PromptSuggestionsWidget` and the prompt-suggestions command-palette toggle are hidden.
- `PromptSuggestionsViaMAA` is in `app/src/bin/neo_warp.rs`'s additional features, so
  organization builds take the credentialed `/ai/passive-suggestions` path. The legacy
  `/ai/generate_am_query_suggestions` handler stays as a fallback when that flag is turned off.
- `AiCommandSearch` (`ai_command_search`): when off, `Input::show_ai_command_search` returns
  early (covering the keybinding, tips and context menu), the `#` trigger and its hint text are
  ignored, the "Open AI Command Suggestions" keybinding/palette entry is disabled, the input
  context-menu item and the Settings `#` trigger toggle and its palette entry are hidden, so
  `generate_commands_from_natural_language` is never called.

The server doesn't check the flags. A client with a flag off never calls the endpoint.

## Testing

Unit tests in `suggestions_tests.rs`, `command_search_tests.rs` and `helpers_tests.rs` use a
scripted `Provider`, like `turn_tests.rs`:

- Shell-command trigger with a JSON reply → `CreateTask` with a `SuggestPrompt` chip. The prompt
  sent to the model includes the command, exit code and output tail.
- `NONE` reply, deprecated trigger, and missing credential → only `Init` and `Finished(Done)`.
- Command search reply wrapped in fences → parsed commands. Parameters not referenced in the
  command are dropped. Invalid JSON → `BAD_PROMPT`.
- The cache returns the latest settings per user, keeps only model and credential fields, keeps
  earlier fields a later request leaves empty, and expires old entries.
- A provider error from command search maps to `OTHER`; a missing key maps to `AI_PROVIDER_ERROR`.
- Next Command (`next_command_tests.rs`): suggestion from context; prefix enforced; destructive
  replies dropped; `NONE` and provider errors answer nothing; a stalled provider times out; the
  cache answers identical requests; a newer prefix request supersedes an older one; the chosen
  model is used when reachable and falls back to the agent model when unknown or keyless;
  sanitizing and the destructive-command list.
- Router tests in `lib_tests.rs`: `/ai/generate_input_suggestions` without a token is 401, without
  remembered settings answers an empty suggestion, and with them answers the scripted command for
  a request shaped like the client's serialization.
- Client (`generate_ai_input_suggestions_tests.rs`): the request serializes `prefix` and `model`
  (omitted when unset), and both server answers deserialize.
- Router tests in `lib_tests.rs`: `GenerateCommands` without remembered settings deserializes
  through the client's `cynic` types as `GenerateCommandsFailure(AiProviderError)`, and the legacy
  endpoint answers `suggestion: null`.
