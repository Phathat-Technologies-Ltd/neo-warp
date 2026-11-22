# Command History Scopes

## Summary

Let the user narrow the command history menu to one of four scopes: Global, Host, Session, and
Directory. The scope is chosen with tabs in the history menu, persists across restarts, and works
the same way in the GUI and the TUI. Everything is local; no cloud service is involved.

## Problem

The up-arrow history menu always shows the full history for the current shell host. In a busy
host the commands a user wants (the ones run a minute ago in this pane, or the ones that are
always run in this repository) are buried among hundreds of others. The opposite need also
exists: a command run on another machine or in another shell cannot be recalled at all.

## Scopes

| Scope | Shows |
| --- | --- |
| Global | Every known command: all hosts, shells, and sessions, plus every shell history file Warp has read. |
| Host | Today's behavior: the shell history file for the current shell host (user, hostname, and shell type) plus commands run on that host in Warp. Commands from other live sessions that started after this one are hidden, as before. |
| Session | Only commands run in the current terminal session. |
| Directory | Commands from the Host list whose recorded working directory equals the current one. |

- For SSH and other remote (warpified) sessions, "host" is the remote host, because the shell host
  is taken from the remote shell's bootstrap.
- Directory compares paths as strings, ignoring a trailing separator. It does not resolve symlinks.
- Prompts (agent view) are not scoped; the scope applies only to shell commands.

## Entries with missing metadata

- Lines read from a shell history file have no session. They never appear in Session.
- A history file line gets a working directory only if Warp has also recorded that command in its
  own database; it then uses the directory of the run Warp kept for that command. Lines with no
  recorded directory never appear in Directory.
- If the current session's working directory is not known yet (shell still bootstrapping),
  Directory is empty.
- Global and Host never drop entries for missing metadata.

## Switching scope

### GUI

- Outside the agent view, the history menu (Up arrow on an empty or first-line input) shows four
  tabs: `Global | Host | Session | Directory`. This replaces the single `All` tab it had before.
- Click a tab, or press Shift-Tab to move to the next tab, as with other inline-menu tabs. The
  existing tab hint in the message bar is shown because the menu now has several tabs.
- When the selected command is still in the new list after a switch, it stays selected.
- Inside the agent view the menu keeps its `All | Commands | Prompts` tabs. The header shows a
  scope selector (`Scope: Host`) to the left of `Configure`. Clicking it, or pressing Ctrl-S while
  the menu is open, moves to the next scope (Global, Host, Session, Directory, then Global again).
  The footer adds `ctrl-s to cycle scope (Host)` before the dismiss hint. The scope filters the
  Commands tab and the commands within All; Prompts and conversations are unaffected.
- Ctrl-S also cycles scope in the classic menu, where it moves the active scope tab. Ctrl-S is
  only bound while the history menu is open, so terminal flow control is unaffected otherwise.

### Source

- Next to the scope, the menu header shows a source selector (`Source: All`): All, Me (commands the
  user ran), or Agent (commands the agent ran). Clicking it, or pressing Ctrl-O while the menu is
  open, moves to the next source. The footer adds `ctrl-o to cycle source (All)` before the dismiss
  hint. This applies in both the agent view and the classic menu, where the source selector sits
  in the header next to `Configure` and the scope stays on the tabs.
- Source filters command rows only; prompts and conversations are unaffected.
- All respects the "include agent commands in history" setting. Choosing Agent always shows the
  agent's commands, since the user asked for them explicitly.
- The chosen source is saved as `terminal.input.history_source` (`all`, `me`, `agent`), local only,
  shared by all panes, default All.

### Warp helper commands

- Commands that call Warp's own shell-integration helpers (any command whose first word is
  `warp_*`, `_warp_*`, or `__warp_*`, such as `warp_run_external_ctrl_r_widget 0`) are never shown
  in any history list. They are dropped when history files are read, when Warp records commands,
  and when persisted commands are loaded.

### Rows

- Each command row is laid out in columns, as in atuin: a fixed-width, right-aligned duration
  (green when it exited 0, red otherwise), the command left-aligned right after it and truncated
  with an ellipsis, then the relative time (`4m ago`) right-aligned at the end. Entries read from
  the shell history file have no duration or exit code, so their duration column is blank but still
  reserved, keeping commands aligned. Prompt and conversation rows have no duration column.
- Typing filters the rows within the current scope; the newest match stays closest to the input.

### TUI

- The history menu header shows the same four tabs, with the active one highlighted.
- Tab moves to the next scope while the history menu is open (Tab is otherwise shell completion,
  which does not apply while the history menu is open).

## Default and persistence

- The default is Host, so nothing changes for users who never switch.
- The chosen scope is saved as the `terminal.input.history_scope` setting
  (`global`, `host`, `session`, `directory`). It is local only and is not synced to the cloud.
- The scope is shared by all panes and both front-ends; every menu opens on the saved scope.

## Inspired by atuin

The menu borrows from atuin's search UI, adapted to Warp's visual style: a visible scope next to the
query that a key cycles through, rows with a colored duration and a relative time, and filtering
that combines the typed query with the scope. Not adopted yet: cycling scope by pressing the menu's
open key again (Up opens the menu and then navigates it, and Ctrl-R opens command search), applying
the scope to Ctrl-R command search, and atuin's alt-1..9 quick-select of the nearest rows.

## Out of scope

- The Ctrl-R command search panel. It keeps showing the Host list. It can adopt the setting later.
- Plain up-arrow cycling without the menu.
- Querying the commands database for older runs in a directory (see TECH.md, follow-ups).
