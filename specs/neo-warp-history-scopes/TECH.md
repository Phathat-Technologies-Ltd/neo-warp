# Command History Scopes: Tech Spec

See `PRODUCT.md` for behavior.

## What exists today

**Persistence.** The SQLite `commands` table (`crates/persistence/src/schema.rs`) already stores,
per executed command: `command`, `exit_code`, `start_ts`, `completed_ts`, `pwd`, `shell`,
`username`, `hostname`, `session_id`, `git_branch`, `cloud_workflow_id`, `workflow_command`, and
`is_agent_executed`. Every field a scope needs is already persisted, so this change adds no
migration.

**In-memory model.** `History` (`app/src/terminal/history.rs`) is a singleton keyed by
`ShellHost` (shell type, user, hostname):

- `history_file_commands`: lines from the host's shell history file, joined with
  `persisted_commands_summary` (one `commands` row per command string) to pick up metadata such as
  `pwd` and `session_id`.
- `session_commands`: commands run in Warp on that host, including restored blocks.
- `commands(session_id)`: history file + session commands for the session's host, minus
  duplicates and minus commands that other live sessions ran after this session started.

`HistoryEntry` carries `session_id`, `pwd`, `start_ts`, `exit_code`, `shell_host`, and more.

**Menus.**

- GUI: `InlineHistoryMenuView` (`app/src/terminal/input/inline_history/view.rs`) with
  `InlineHistoryMenuDataSource`. Both call
  `History::up_arrow_suggestions_for_terminal_surface` (`history/up_arrow.rs`), which sorts and
  dedupes. Tabs are `InlineMenuTabConfig<HistoryTab>`; Shift-Tab calls `select_next_tab`.
- TUI: `TuiPromptAndCommandHistoryMenuModel`
  (`crates/warp_tui/src/prompt_and_command_history_menu.rs`) via `tui_up_arrow_history`
  (`app/src/tui_export/history.rs`), the same combiner.
- Ctrl-R (`app/src/search/command_search/history`) uses `commands_shared` and is unchanged.

## Design

### Scope type

`HistoryScope` (`app/src/terminal/history/scope.rs`): `Global`, `Host` (default), `Session`,
`Directory`, with `ALL`, `next()`, `label()`, and `includes(entry, session_id, current_pwd)`.
`includes` is the per-entry filter: Session needs `entry.session_id == current`, Directory needs
both pwds present and equal after trimming a trailing separator.

### Data layer

`History::commands_in_scope(session_id, scope, current_pwd)`:

- Host, Session, Directory start from `commands(session_id)` so they keep its dedupe and
  cross-session rules, then filter with `includes`.
- Global starts from every host's history file and session commands (`all_commands`).

`UpArrowHistoryConfig` gains `scope`, and `up_arrow_suggestions_for_terminal_surface` takes
`current_pwd`. Existing sort and dedupe run on the scoped list. Plain up-arrow cycling
(`collate_ai_and_command_history`) passes `Host` and no pwd, so it behaves as before.

The current directory comes from `ActiveSession::current_working_directory()`, which is updated on
each precmd for local and remote sessions alike.

### Setting

`InputSettings::history_scope` (`terminal.input.history_scope`, `SyncToCloud::Never`, surface
`ALL`). Data sources read it on every query, so a change from any pane applies the next time a
menu queries.

### GUI

- `HistoryTab::Scope(HistoryScope)`; outside the agent view `build_tab_configs` returns one tab per
  scope with no `QueryFilter` (scope is not a `QueryFilter`, so the many exhaustive matches on that
  enum are untouched).
- The view opens on the saved scope (`InlineMenuView::new_with_tabs(.., Some(initial_tab))`), and
  re-syncs the active tab from the setting each time the menu opens and after agent-view
  transitions (`InlineMenuModel::set_active_tab_id`, which does not emit a tab-change event).
- On `InlineMenuEvent::TabChanged` with a scope tab, the view writes the setting, then reruns the
  query; the existing selection-restore logic keeps the selected command when present.

- In the agent view (tabs carry type filters, `tabs_select_item_type`), the header's trailing
  element renders a scope `ActionButton` before `Configure`. Its label is kept in sync with the
  setting through an `InputSettingsChangedEvent::HistoryScopeSetting` subscription, so another
  pane's change shows up too.
- `InputAction::CycleHistoryScope` is bound to `ctrl-s` (`HISTORY_SCOPE_CYCLE_KEYSTROKE`) under the
  `InlineHistoryMenuOpen` Input context flag; the selector's click dispatches the same action.
  `InlineHistoryMenuView::cycle_scope` persists `next()`, moves a scope tab if there is one, and
  reruns the query with selection restore. The data source already reads the setting per query.
- `AcceptHistoryItem::produce_inline_menu_message` appends the scope hint when the tabs carry type
  filters.
- Rows: `CommandRun::from_entry` derives duration and success from `start_ts`, `completed_ts`, and
  `exit_code`; `InlineHistoryItem` renders it in a fixed-width column before the command.

### TUI

- The menu snapshot header lists the four scopes as `TuiInlineMenuTab`s; the row budget accounts
  for the tab row.
- `TuiInlineMenuHandle` gains `has_tabs` and `cycle_tab` (default no-op); the history menu's handle
  forwards to `cycle_scope`, which advances and persists the setting, refreshes rows, and previews
  the selection.
- `TuiInputView` adds `tui:input:cycle_inline_menu_tab` on Tab, gated on a new
  `TuiInlineMenuHasTabs` context flag. Shell completion's Tab binding is disabled while that flag
  is set so the two never compete.

## Testing

- `history/scope_tests.rs`: `includes` for each scope, missing-metadata cases, trailing
  separators, `next()` cycle.
- `history_tests.rs::test_commands_in_scope`: two hosts and two sessions on one host; checks Host,
  Session, Directory (per session), and Global.
- `inline_history/view_tests.rs`: scope tabs outside the agent view, type tabs inside it, selector
  visibility, selector label, cycle order.
- `inline_history/data_source_tests.rs`: footer scope hint keystroke and text.
- `inline_history/search_item_tests.rs`: duration formatting and `CommandRun` from entries with and
  without metadata.
- `warp_tui` `prompt_and_command_history_menu_tests.rs`: cycling refilters rows, moves the selected
  header tab, and persists the setting.

## Follow-ups

- Directory accuracy for history file lines: `persisted_commands_summary` keeps one row per
  command string, so a command run in several directories matches only the directory of the kept
  row. A precise version would load all `(command, pwd)` pairs for the host from `commands`.
- Apply the setting to Ctrl-R command search.
- Cycle scope by pressing the menu's open key again; alt-1..9 quick-select (atuin-style).
