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
- Inside the agent view the menu keeps its `All | Commands | Prompts` tabs; commands in it use the
  saved scope.

### TUI

- The history menu header shows the same four tabs, with the active one highlighted.
- Tab moves to the next scope while the history menu is open (Tab is otherwise shell completion,
  which does not apply while the history menu is open).

## Default and persistence

- The default is Host, so nothing changes for users who never switch.
- The chosen scope is saved as the `terminal.input.history_scope` setting
  (`global`, `host`, `session`, `directory`). It is local only and is not synced to the cloud.
- The scope is shared by all panes and both front-ends; every menu opens on the saved scope.

## Out of scope

- The Ctrl-R command search panel. It keeps showing the Host list. It can adopt the setting later.
- Plain up-arrow cycling without the menu.
- Querying the commands database for older runs in a directory (see TECH.md, follow-ups).
