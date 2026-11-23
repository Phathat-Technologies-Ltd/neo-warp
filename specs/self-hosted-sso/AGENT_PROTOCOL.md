# Multi-agent protocol: the minimum a self-hosted server must speak

This covers what a server needs to drive the Warp client's agent through a plain-text turn, a follow-up turn, client-executed tools, and errors. Facts come from the client source and the proto at `warp-proto-apis@7e78c26` (`~/.cargo/git/checkouts/warp-proto-apis-2098b3a955068931/7e78c26/apis/multi_agent/v1/`, cited below as `proto/<file>:<line>`). Conclusions marked **INFERENCE** are not verified against code.

Rust types live in crate `warp_multi_agent_api` (prost, `use warp_multi_agent_api as api;`). Each nested message or oneof becomes a snake_case module, e.g. `api::response_event::Type::Init`, `api::client_action::Action::AddMessagesToTask`, `api::message::Message::AgentOutput`, `api::message::tool_call::Tool::RunShellCommand`. The proto package is `warp.multi_agent.v1`.

## 0. Transport

| Item | Value | Source |
|---|---|---|
| Endpoint | `POST {server_root_url}/ai/multi-agent`. Passive suggestions go to `/ai/passive-suggestions`; ignore them or return an empty `Done` stream. | `crates/warp_multi_agent_client/src/lib.rs:135-150` |
| Request body | Raw protobuf `api::Request`, `Content-Type: application/x-protobuf` | `crates/http_client/src/lib.rs:516-530` |
| Auth | `Authorization: Bearer <access token>` (the same token as other server calls), plus optional team-UID / ambient headers | `lib.rs:53-78` |
| Response | `200`, `Content-Type: text/event-stream`. Each SSE `data:` line is **padded** URL-safe base64 (`BASE64_URL_SAFE` requires `=` padding) of a protobuf `api::ResponseEvent`. Surrounding `"` are tolerated. The `event:`/`id:` fields are ignored. | `lib.rs:80-89, 161-167`; base64 `PAD` config requires padding |
| End of stream | After sending `Finished`, close the response. A clean EOF ends the client stream (`StreamEnded` is swallowed). | `crates/http_client/src/lib.rs:581-586` |

Wrong content type, or a decode failure, counts as a recoverable error: the client silently retries the same request up to 3 times (§5).

## 1. Scenario: first turn, plain-text answer

### What the client sends
- `Request.metadata.conversation_id = ""` (new conversation) (`app/src/ai/agent/api/impl.rs:122-128`).
- `Request.task_context.tasks = []`. The client keeps an optimistic root task with a client UUID, but `compute_active_tasks` sends only tasks that have a server-provided source (`app/src/ai/agent/conversation.rs:1568-1586`). The optimistic id never reaches the server.
- `Request.input = Input { context: InputContext, type: UserInputs { inputs: [ UserInput { input: UserQuery { query, referenced_attachments, mode, intended_agent, origin } } ] } }` (`app/src/ai/agent/api/convert_to.rs:57-288, 341-360`).
  - The deprecated `Input.user_query = 2` is never used.
  - `Input.context` (`proto/input_context.proto:18-35`) carries the cwd (`directory.pwd`, `directory.home`), `operating_system.platform`, `shell.name`/`version`, `current_time`, `git`, `project_rules`, selected text, and images. Use it for the system prompt.

### What the server must stream (in this order)

```
1. ResponseEvent{ init: StreamInit{ conversation_id: <new non-empty id>, request_id: <new id>, run_id: "" } }
2. ResponseEvent{ client_actions: ClientActions{ actions: [
       CreateTask{ task: Task{ id: ROOT } },                                  // first turn only
       AddMessagesToTask{ task_id: ROOT, messages: [ Message{ id: U1, task_id: ROOT, request_id, timestamp,
                                                    user_query: UserQuery{ query, context } } ] }  // echo (see §2)
   ] } }
3. ResponseEvent{ client_actions: [ AddMessagesToTask{ task_id: ROOT, messages: [
       Message{ id: M1, task_id: ROOT, request_id, timestamp, agent_output: AgentOutput{ text: "<first chunk>" } } ] } ] }
4. ResponseEvent{ client_actions: [ AppendToMessageContent{ task_id: ROOT,
       message: Message{ id: M1, agent_output: AgentOutput{ text: "<delta>" } },
       mask: FieldMask{ paths: ["agent_output.text"] } } ] }      // repeat per delta
5. ResponseEvent{ finished: StreamFinished{ done: Done{} } }
```

Several actions may share one `ClientActions` event. Batching deltas is fine.

**StreamInit** (`proto/response.proto:32-57`), handled in `app/src/ai/agent/conversation.rs:2399-2429`:
- `conversation_id` is stored without validation as `server_conversation_token` and echoed back in `metadata.conversation_id` on every later request (`impl.rs:124-128`). **It must be non-empty.** `""` is accepted, but every later request would then look like a new conversation.
- `request_id` is used for telemetry and output ids (`response_stream.rs:855-861`). It can be any string; a UUID is a good choice.
- `run_id` can be empty: empty values are filtered, and a non-UUID value is ignored.
- **Init must come before any ClientActions in every stream, including follow-ups.** It is what creates the streaming output (`init_output`). Without it, `AddMessagesToTask` fails with `OutputNeverInitialized` (`app/src/ai/agent/task.rs:1025-1035`), and at the end the client shows "Warp lost connection while receiving the agent response…" (`controller.rs:4330-4350`).

**Root task: the server creates it.**
- `AddMessagesToTask` on the optimistic id fails, because the task has no source (`task.rs:637-639`, `conversation.rs:3308-3311`).
- A `CreateTask` whose `dependencies` is unset or has an empty `parent_task_id` (`app/src/ai/agent/task/helper.rs:10-15`) upgrades the client's current root task in place:
  - The task id becomes `task.id` and the proto becomes its source (`conversation.rs:3097-3126`, `task.rs:166-231`).
  - The old optimistic id stays as an alias for lookup.
- **Send the root CreateTask only once per conversation.** A second one hits `UnexpectedUpgrade` and drops the root from the client store (`task.rs:210`, from reading the code).
- On later turns, the root id is `task_context.tasks[i].id` where `dependencies.parent_task_id` is empty.
- Any messages inside `CreateTask.task.messages` are applied to the exchange (`task.rs:213-229`). Do not re-send them via `AddMessagesToTask`, or they appear twice.
- The client's ACP harness does exactly this sequence: `CreateTask{Task{id}}` followed by an `AddMessagesToTask` that echoes the UserQuery (`app/src/ai/agent_sdk/driver/harness/acp/mapping.rs:367-410`). It is the best reference implementation.

**Message fields** (`proto/task.proto:310-365`):
- Set `id` (unique, e.g. a UUID), `task_id`, `request_id` (= Init's) and `timestamp` (`mapping.rs:840-849`).
- None of them are validated, but:
  - `id` is the key for later Append/Update calls. Duplicates match the first occurrence; an empty id collides with others.
  - `timestamp` drives the exchange's finish time (`conversation.rs:2872`).
- An unknown `task_id` on the action gives `TaskNotFound`. Errors are only reported to Sentry, and the rest of that `ClientActions` event is skipped (`history_model.rs:2077-2090`, `controller.rs:4061-4066`). The user sees nothing.

**Incremental text** (`crates/field_mask/src/lib.rs`, `conversation.rs:3486-3535`, `task.rs:750-789`):
- `AppendToMessageContent` finds the message by `message.id` in the task, then for each mask path concatenates the patch's string leaf onto the existing value.
- Paths are proto field names relative to `Message`, with the oneof member named directly: `agent_output.text` and `agent_reasoning.reasoning` (`mapping.rs:37-39`).
- An unknown path is a silent no-op. A non-string leaf gives `UnsupportedAppend`.
- `mask` and `message` must both be `Some`, otherwise the action is ignored ("unsupported client action").
- The message must already exist from an earlier `AddMessagesToTask` in the same stream (`ExchangeNotFound` / `MessageNotFound` otherwise).
- `UpdateTaskMessage{task_id, message, mask}` replaces the masked fields. If the id is unknown, it inserts the message (`conversation.rs:3401-3485`). You don't need it for text.
- Start a new message id when the kind changes (text → tool call → text).

**Reasoning (optional):**
- `Message.agent_reasoning{reasoning, finished_duration}` is rendered when `settings.supports_reasoning_message` is true (it always is, `impl.rs:94`).
- Stream it the same way, with path `agent_reasoning.reasoning`.

**StreamFinished** (`proto/response.proto:61-89`): `reason = done` (or unset) means success. See §5 for the other values. Every other field (`token_usage`, `conversation_usage_metadata`, `request_charges`, `should_refresh_model_config`) is optional.

**Transactions are not needed.**
- Without `BeginTransaction`, every action applies immediately (`conversation.rs:2936-2945, 4129-4198`).
- If you use them:
  - Rollback restores task snapshots, but only actions that checkpoint are covered (CreateTask, AddMessagesToTask, UpdateTaskDescription, MoveMessagesToNewTask).
  - A stream that errors or is cancelled *commits* an open transaction.
  - A transaction left open after `Done` stays open (INFERENCE).
- Recommendation: don't emit them.

## 2. Scenario: follow-up turn

The client sends:
- `metadata.conversation_id` = the Init id.
- `task_context.tasks` = every server-sourced task, **verbatim**. That includes all messages the server added to `task.messages`, plus `Task.server_data` and `Message.server_message_data` untouched (`conversation.rs:1568-1586`, `controller.rs:3397, 3463-3465`).
- `input` = `UserInputs[UserQuery]` as in §1.

**The client never adds its own inputs (UserQuery or ToolCallResult) to `task.messages`.** Only messages the server sends via `CreateTask`/`AddMessagesToTask`/`UpdateTaskMessage` enter `task.source.messages` (`task.rs:630-648`; `conversation.rs:3138-3391`).

So if the server keeps no state, it must echo the inputs back so they appear in the history next time:
- Echo each UserQuery as `Message.user_query` at the start of the stream, as the real server does ("Echoes a user prompt into the task, as the MAA server does", `mapping.rs:386-390`).
- Echo each tool result as `Message.tool_call_result` (see §3).

Echoed `UserQuery`, `ToolCallResult`, `ServerEvent`, `SystemQuery` and similar messages are not rendered (`app/src/ai/agent/api/convert_from.rs:593-607`). They exist only for the history.

To rebuild Anthropic `messages[]`, walk `root.messages` in order:

| `api::message::Message` variant | Anthropic content |
|---|---|
| `UserQuery{query}` | user text block |
| `AgentOutput{text}` | assistant text block |
| `AgentReasoning` | drop, unless you keep the thinking signature in `server_message_data` (INFERENCE: viable, since the field is round-tripped verbatim) |
| `ToolCall{tool_call_id, tool}` | assistant `tool_use{id: tool_call_id, …}` |
| `ToolCallResult{tool_call_id, result}` | user `tool_result{tool_use_id}` |

Then append the new `Input` as the final user turn.

What the server must preserve:
- `conversation_id`: always echo the request's value in `StreamInit`.
- Task ids: keep adding to the same root id, and do not send another root CreateTask.
- `server_data`: opaque storage via `UpdateTaskServerData{task_id, server_data}` (`conversation.rs:3392-3400`), round-tripped forever.

The client persists tasks to sqlite (`conversation.rs:4083-4086`), so after a restart, history comes back only through `task_context`.

## 3. Scenario: RUN_SHELL_COMMAND

### Server → client
Use one Message per tool call (`Message.message` is a single oneof). Several tool calls can go in one `AddMessagesToTask`.

```
AddMessagesToTask{ task_id: ROOT, messages: [ Message{ id: M2, task_id: ROOT, request_id, timestamp,
  tool_call: ToolCall{
    tool_call_id: "toolu_…",                      // reuse the Anthropic tool_use id
    run_shell_command: RunShellCommand{
      command: "ls -la",
      is_read_only: true,                         // used for auto-approve
      is_risky: false,                            // used when AgentDecides + flag
      uses_pager: false,
      // wait_until_complete unset ⇒ wait for completion
      risk_category: RISK_CATEGORY_READ_ONLY      // ignored by this client; set for forward compat
    } } } ] }
then Finished{ done }
```

Parsing (`app/src/ai/agent/api/convert_from.rs:640-819`, `crates/ai/src/agent/action/convert.rs:21-41`):
- `id = tool_call_id`, with no empty check. Use a unique, non-empty value: it keys the result.
- `tool: None` gives `MissingTool`, which fails the whole message batch.
- The `risk_category` field is never read. `is_read_only` and `is_risky` feed permissions (`app/src/ai/blocklist/permissions.rs:912-1030`):
  1. The denylist is checked first, then the allowlist.
  2. Then, depending on the user's setting, `is_risky == false` may auto-run (behind `AgentDecidesCommandExecution`), redirections need confirmation, and otherwise `is_read_only` decides.
  3. If the command is not auto-approved, the user confirms it in the UI.
  4. An unsupervised/autonomous run instead gets a `PermissionDenied` result (`execute.rs:712-722`).
- The client ignores `wait_until_complete`: any command still running after 2 s (`ShellCommandExecutor::MAX_WAIT_DURATION`) comes back as a `long_running_command_snapshot`. The server then opens a CLI subagent for it: a `Subagent{task_id, cli{command_id}}` tool call in the root task, then `CreateTask` with `parent_task_id = root`. While that call has no result, the client marks the block agent-monitored, routes follow-ups to the subtask (`UserQuery.intended_agent = CLI`, or `CliAgentUserQuery` with a snapshot when no subagent exists yet, which upgrades the client's optimistic subtask on that `CreateTask`), and runs `ReadShellCommandOutput` / `WriteToLongRunningShellCommand` calls. A `ToolCallResult{subagent}` in the root closes it (`crates/selfhost_server/src/agent/monitor.rs`). A follow-up sent while the agent polls, and a take over, cancel the pending `ReadShellCommandOutput` on the client without reporting a result; the server answers it as cancelled on the next query or `ResumeConversation` (hand back targets the subtask with the running block; a command that exits during a take over resumes the root with the finished block). The server caps polls at 30 s so the agent keeps getting the turn back. Take over hides the subagent's panel on the block by design (`CLISubagentView::render` returns empty while the user is in control, `app/src/ai/blocklist/block/cli.rs`).

**How the turn ends:** send `Finished{done}`.
- `mark_request_completed` sees new actions, so the turn does not end (`conversation.rs:2550-2585`).
- `AfterStreamFinished` queues the actions (`controller.rs:4280-4355` → `action_model.queue_actions`). This happens **only on `Done`**; actions from a stream that errored are not run.
- Once every action finishes, the client sends a follow-up Request automatically (`controller.rs:514-656, 2478-2540`).

**Execution order:**
- Read-only tools run in parallel: ReadFiles, SearchCodebase, ReadSkill, and Grep/FileGlob where supported (`action_model/execute.rs:469-490`).
- Everything else runs serially.
- Results come back in the original action order (`action_model.rs:209-244`).

### Client → server (next Request)
`task_context` now contains the ToolCall message. The input is:

```
Input{ context, user_inputs: UserInputs{ inputs: [
  UserInput{ tool_call_result: request::input::ToolCallResult{
     tool_call_id: "toolu_…",
     run_shell_command: RunShellCommandResult{
        command: "<command actually run; user may have edited it>",
        command_finished: ShellCommandFinished{ output, exit_code, command_id, start_ts, finish_ts } } } } ] } }
```

(`app/src/ai/agent/api/convert_to.rs:669-784`, `crates/ai/src/agent/action_result/convert.rs:106-236`.) The deprecated `Input.tool_call_result = 3` and the top-level `RunShellCommandResult.output` / `exit_code` are never set.

Other `RunShellCommandResult.result` variants:
- `long_running_command_snapshot{command_id, output, cursor, is_alt_screen_active, …}`
- `permission_denied{denylisted_command}`
- `terminal_busy{running_command_id}`

Ctrl-C arrives as `command_finished` with exit 130.

**Echo for history:** convert the request result into `Message.tool_call_result` and add it with `AddMessagesToTask` at the start of the next stream (after Init). The ACP harness does the same (`mapping.rs:756-790`). Note that `request::input::ToolCallResult` and `message::ToolCallResult` are **different messages with different field numbers** (`proto/request.proto:184-229` vs `proto/task.proto:1262-1316`), so map variant by variant (e.g. `run_shell_command` is field 2 in both, but `file_glob_v2` is 15 vs 19). Only the message form has `cancel` (`google.protobuf.Empty`, field 14).

**Cancel/reject:** if the user rejects a command, the client sends **no result at all**.
- Cancelled results convert to `ConvertToAPITypeError::Ignore` and are dropped (`action_result/convert.rs`; `convert_to.rs:263, 272`).
- No follow-up is sent automatically; the conversation is marked cancelled (`action_result/mod.rs:1060`).
- The next user prompt then arrives with a ToolCall in history that has no matching result.
- The server must synthesize a tool_result (e.g. "user cancelled") for every unanswered `tool_use` before calling Anthropic, which rejects a `tool_use` without a result.
- **INFERENCE:** it should also echo a `Message.tool_call_result{cancel}` so the history stays consistent.

## 4. Other client tools

The same rules apply to all of them: one Message per call, `Finished{done}`, and the result returns as `UserInputs[ToolCallResult{tool_call_id, …}]`. Paths may be relative (resolved against the session cwd, with `~` expanded) or absolute (`crates/ai/src/paths.rs:80`). Results report resolved absolute paths.

| Tool (`ToolType`) | Call: `message::tool_call::Tool::…` | Result: `request::input::tool_call_result::Result::…` |
|---|---|---|
| `READ_FILES` | `ReadFiles{ files: [File{ name, line_ranges: [FileContentLineRange{start,end}] }] }`. Empty `line_ranges` reads the whole file. | `ReadFiles(ReadFilesResult{ any_files_success{ files: [AnyFileContent{ text_content: FileContent{file_path, content, line_range?} \| binary_content{file_path, data} }], failed_reads: [{path, message}] } \| error{message} })`. The client always uses `any_files_success`, never `text_files_success` (`action_result/convert.rs:296-331, 995-1025`). |
| `APPLY_FILE_DIFFS` | `ApplyFileDiffs{ summary, diffs: [FileDiff{file_path, search, replace}], new_files: [NewFile{file_path, content, allow_overwrite}], deleted_files: [{file_path}], v4a_updates }` | `ApplyFileDiffs(ApplyFileDiffsResult{ success{ updated_files_v2: [{ file: FileContent, was_edited_by_user }], deleted_files: [{file_path}] } \| error{message} })`. `FileContent` holds the changed region plus context, not the whole file (`convert.rs:405-449`). |
| `GREP` | `Grep{ queries: [..], path }` (empty queries is an error) | `Grep(GrepResult{ success{ matched_files: [{ file_path, matched_lines: [{line_number}] }] } \| error })`. **Line numbers only, no line text** (`convert.rs:502-526`). |
| `FILE_GLOB` (deprecated) | `FileGlob{ patterns, path }` (empty path means `.`) | `FileGlob(FileGlobResult{ success{ matched_files: "<newline-joined>" } \| error })` |
| `FILE_GLOB_V2` | `FileGlobV2{ patterns, search_dir, max_matches, max_depth, min_depth }` | `FileGlobV2(FileGlobV2Result{ success{ matched_files: [{file_path}], warnings } \| error })` |

### ApplyFileDiffs semantics
`crates/ai/src/agent/action/convert.rs:79-135`; `app/src/ai/blocklist/action_model/execute/request_file_edits/diff_application.rs:310-700`; `crates/ai/src/diff_validation/mod.rs:598-1047`.

- **Search/replace (`diffs`) is the format to use.** It is not unified diff.
  - `search` is matched in this order: exact, then indentation-agnostic, then Jaro-Winkler at a 0.9 threshold. Leading `N|` line-number prefixes act as location hints.
  - An unmatched block fails the whole call with `error{message}`.
- **Empty `search`:**
  - On an existing file, it *prepends* `replace`.
  - On a missing file, exactly one empty-search diff creates the file.
- **New files:** use `new_files` (`settings.supports_create_files = true` is always sent, `impl.rs:84`). `allow_overwrite` is honoured (`supports_create_file_overwrite = true`). Creating an existing file without it gives `AlreadyExists`.
- Edits apply in this order: diffs, v4a, new_files, deleted_files. Creating the same file twice, or deleting and also editing it, is an error.
- `v4a_updates` (OpenAI apply-patch hunks) is accepted only when `settings.supports_v4a_file_diffs` is true (feature-flagged). Not needed.
- Anthropic `str_replace_based_edit_tool` maps naturally:
  - `str_replace` → `diffs[{file_path, search: old_str, replace: new_str}]`
  - `create` → `new_files[{file_path, content: file_text, allow_overwrite: true}]`
  - `view` → READ_FILES
  - `insert` has no direct equivalent: emulate it with search/replace on the anchor line.
- The client sends `use_anthropic_text_editor_tools: false` (`impl.rs:82`). That flag only concerns the hosted server and does not change the client wire format.

### ReadFiles line-range gotcha
The proto says `[start, end]` is inclusive and 1-indexed (`proto/file_content.proto:12`). The client maps it to `start..end` and treats `end` as **exclusive** (`action/convert.rs:214-225`; `crates/warp_files/src/text_file_reader.rs:115`, test `text_file_reader_tests.rs:69-81`). To get lines a..=b, send `end = b + 1`.

Results arrive in action order. With parallel calls, all results come in one Request, in `inputs[]`. Return them to Anthropic as a single user message containing every `tool_result`.

## 5. Errors

**Preferred: a graceful stream ending with a reason** (`app/src/ai/blocklist/controller.rs:4458-4646`, text in `app/src/ai/blocklist/view_util.rs:109-210`, `ERROR_APOLOGY_TEXT` at :30):

| `StreamFinished.reason` | What the user sees |
|---|---|
| `done{}` / unset | success |
| `internal_error{message}` | "I'm sorry, I couldn't complete that request." plus "Response stream finished unexpectedly with internal error: {message}". **Use this for Anthropic API errors**, with a readable message. |
| `llm_unavailable{}` | "The LLM is currently unavailable." (for Anthropic 529/overloaded) |
| `invalid_api_key{provider: LLM_PROVIDER_ANTHROPIC, model_name}` | Dedicated card: "Failed to authenticate with {provider} when using {model}…" (for Anthropic 401) |
| `context_window_exceeded{}` / `max_token_limit{}` | "Input exceeded context window limit." |
| `quota_limit{}` | Credit-limit card mentioning a reset date. Avoid it: the wording is Warp-specific. |
| `other{}` | "Response stream finished unexpectedly (with finish reason `Other`)." |

Notes:
- Send `Init` first even on error. Any text already streamed stays visible.
- Every non-Done reason marks the request errored (`conversation.rs:2745-2843`). Tool calls from that stream are **not** executed.
- Do not send an AgentOutput message containing error text instead: it would render as normal assistant text, and the turn would count as successful.

**HTTP-level errors** (`app/src/server/server_api.rs:300-357`):
- A non-200 response becomes `AIApiError::ErrorStatus(status, body)` and renders as "Request failed with error: ErrorStatus(…, \"<body>\")" (Debug format, `app/src/ai/agent/mod.rs:967-976`).
- 4xx other than 408/429 is not retried.
- 5xx, transport errors, wrong content type, and decode errors are retried silently up to 3 times with backoff (`response_stream.rs:36, 187-214`). After ClientActions have been received, the client sends a `ResumeConversation` request instead (`Input.type = resume_conversation`). The server should handle that input by continuing from history.
- 429 maps to "Warp is currently overloaded…" unless `X-Warp-Error-Code` indicates out-of-credits.
- Use HTTP errors only for pre-stream failures such as auth. Use `StreamFinished` for model errors.

**Stream ends without `Finished`:**
- The client treats it as `UnexpectedEof`: it retries (before any actions) or resumes (after actions), and finally shows "Warp lost connection while receiving the agent response. This is usually temporary." (`response_stream.rs:907-938`, `agent/mod.rs:957-961`).
- **Always send `Finished`.**

## 6. Settings that matter

All are in `api::request::Settings` (`proto/request.proto:398-545`), set in `app/src/ai/agent/api/impl.rs:70-121`.

| Field | Client value | Meaning for the server |
|---|---|---|
| `model_config.base` | the selected `LLMId` (`impl.rs:72`) | The model to call: a picker id from the server's model list, or a `custom_model_providers[].models[].config_key`. `coding` is not set by this client (it is in `RequestParams` but not copied). |
| `model_config.base_model_context_window_limit` | 0 = default | Optional input-token cap |
| `api_keys.anthropic` | Sent only if BYO keys are enabled for the user/workspace (`app/src/ai/agent/api.rs:343-354`; `crates/ai/src/api_keys.rs:1262-1340`). The field is absent when no key is set. | **It may be an Anthropic OAuth access token rather than an API key** (`send_anthropic_oauth_as_api_key`, `api_keys.rs:1268-1277`). The server must detect this and use the Bearer + OAuth beta header form. |
| `supported_tools` | Grep, FileGlob, FileGlobV2, ReadMcpResource, CallMcpTool, InitProject, OpenCodeReview, RunShellCommand, SuggestNewConversation, Subagent, WriteToLongRunningShellCommand, ReadShellCommandOutput, Read/Create/EditDocuments, SuggestPrompt, InsertReviewComments, plus ReadFiles, ApplyFileDiffs and SearchCodebase **only for local sessions** (or connected remote sessions), plus flag-gated extras (`impl.rs:225-303`) | Only emit tools in this list. An SSH session without a connected remote server has no ReadFiles or ApplyFileDiffs. |
| `supports_parallel_tool_calls` | always `true` | Several ToolCall messages per turn are fine |
| `supports_create_files` / `supports_create_file_overwrite` | `true` / `true` | Use `new_files` with `allow_overwrite` |
| `supports_v4a_file_diffs` | flag-dependent | Ignore; use `diffs` |
| `use_anthropic_text_editor_tools` | `false` | Server-side only; ignore |
| `supports_reasoning_message` | `true` | `AgentReasoning` messages render |
| `autonomy_level` | `SUPERVISED` / `UNSUPERVISED` | When unsupervised, there is no user to confirm commands |
| `rules_enabled`, `web_search_enabled`, `planning_enabled`, `warp_drive_context_enabled`, `supports_todos_ui`, … | various | Safe to ignore for a minimal server |
| `custom_model_providers` | user-configured endpoints | Optional |

Metadata: `metadata.conversation_id`, `forked_from_conversation_id` (new conversations only), `logging` map (`is_autodetected_user_query`, `entrypoint`, `is_auto_resume_after_error`).

## 7. Surprises and risks

1. Init is required in **every** stream, before any action. The server must CreateTask the root exactly once; the client never sends its optimistic id.
2. The client keeps no copy of its own inputs. A stateless server must echo UserQuery and ToolCallResult messages, or they vanish from the next `task_context`.
3. Rejected or cancelled tool calls produce **no** result in the next request. The server must patch the history before calling Anthropic.
4. Request-side and message-side `ToolCallResult` have different field numbers, and only the message side has `cancel`.
5. ReadFiles `end` is exclusive in practice, despite the proto comment.
6. Grep returns line numbers only. A model expecting matching lines will need a follow-up ReadFiles, or the server could expose "grep" as a shell command instead.
7. Action errors (unknown task id, missing message id, bad mask) never reach the user; the rest of that `ClientActions` event is skipped silently. Watch client logs and Sentry when debugging.
8. Base64 must be padded URL-safe. Standard base64 with `+` or `/` fails to decode and triggers silent retries.
9. `api_keys` is absent unless BYO keys are enabled by workspace policy. A self-hosted server needs its own key fallback or must fail with `invalid_api_key`.
10. `ResumeConversation` input arrives after a mid-stream failure (§5). Treat it as "continue from history".
