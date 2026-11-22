# neo-warp feature flags

## Summary

Organization (neo-warp) builds run without Warp's backend. Many Warp features only work against
that backend (Warp Drive, settings sync, cloud agents, sharing, billing). Admins need one place to
turn product areas on or off for their organization, and users must never see a feature that is
off: no menu item, settings page, widget, slash command, palette entry, banner, or panel, and no
network call to a server that will not answer.

## Behavior

1. Organization builds evaluate a flag file in the flagd flag-definition format
   (`https://flagd.dev/schema/v0/flags.json`) in-process at startup. No flagd daemon runs.
2. Flags are resolved from these sources, each later source overriding the earlier one flag by
   flag:
   1. The defaults shipped with the app.
   2. The admin file `flags.flagd.json` next to `organization.toml`:
      - macOS: `/Library/Application Support/Warp/flags.flagd.json`
      - Windows: `%ProgramData%\Warp\flags.flagd.json`
      - Linux: `/etc/warp/flags.flagd.json`
   3. In debug builds only, the file named by the `WARP_ORG_FLAGS_FILE` environment variable.
3. A flag whose `state` is `DISABLED`, or that a file leaves out, keeps the value of the earlier
   source. A file that fails to parse is ignored as a whole, with an error in the log, and startup
   continues. Unknown flag names are ignored with a warning.
4. Builds that are not organization builds ignore every flag file and keep upstream behavior:
   every product-area flag stays on. The "Default" column below applies only to organization
   builds.
5. Flags take effect at startup. Changing the file requires restarting the app.

## Flags

| Flag | Default | Controls |
| --- | --- | --- |
| `warp_drive` | off | Warp Drive panel and objects: workflows, notebooks, env var collections, Drive-backed rules UI |
| `settings_sync` | off | Cloud settings sync, including `UpdateUserSettings` calls |
| `cloud_agents` | off | Oz/ambient/scheduled agents, runners, cloud environments, agent API keys, handoff |
| `warp_hosted_mcp` | off | Warp-hosted MCP servers and the MCP gallery. Local MCP servers stay |
| `sharing_and_teams` | off | Block/object sharing, guests, teams, invites, session sharing |
| `billing_and_referrals` | off | Billing, credits, referrals, upgrade prompts, onboarding survey |
| `block_titles` | off | AI-generated titles for shared blocks (`/ai/generate_block_title`) |
| `conversation_cloud_sync` | off | Cloud conversation history and sharing. Local history stays |
| `voice_input` | on | Voice input |
| `voice_hands_free` | on | Hands-free voice input (needs `voice_input`; off whenever `voice_input` is off) |
| `codebase_indexing` | on | Codebase indexing and embedding, including the embedded server's index |
| `ai_prompt_suggestions` | on | AI prompt suggestions |
| `ai_command_search` | on | AI command search |

A flag that is off is a hard override: the product-area flag and the upstream flags that drive its
feature stay off for the whole process. Server experiments, user preferences, and runtime overrides
cannot turn them back on.

### What an off flag hides today

- `warp_drive`: the Warp Drive panel, its settings page, and Drive objects as AI context.
- `settings_sync`: cloud settings sync and its settings controls.
- `cloud_agents`: cloud mode, ambient and scheduled agents, handoff, cloud environments, runners,
  and their settings pages.
- `warp_hosted_mcp`: Warp-hosted MCP servers in the MCP gallery.
- `sharing_and_teams`: session sharing, block sharing from the terminal, the shared blocks and
  teams settings pages, and the pane header share button with its "Share pane" command palette
  entry and keybinding (Drive objects, sessions, and conversations).
- `billing_and_referrals`: billing, usage, and referrals settings pages, the workspace upgrade
  prompts gated on the flag, and the resource center "Invite a friend" button.
- `block_titles`: shared block title generation requests.
- `conversation_cloud_sync`: cloud conversation storage, its privacy setting toggle, and every
  conversation share entry point: the pane header share button and its "not stored in the cloud"
  tooltip, the AI block "Copy share link" and "Share conversation" menu items, and the
  conversation list "Share conversation" item. Conversation sharing also requires
  `sharing_and_teams`.
- `codebase_indexing`: client indexing and embedding (`FullSourceCodeEmbedding`,
  `CodebaseIndexPersistence`, `CodebaseIndexSpeedbump`, `CrossRepoContext`,
  `RemoteCodebaseIndexing`) and the embedded server's codebase index.

### Known gaps

These entry points are not yet hidden when their flag is off. They fail against the organization
server instead of disappearing:

- `warp_drive`: workflow, notebook, and env var collection entry points outside the Drive panel
  (command palette, slash commands, block context menus).
- `sharing_and_teams`: Drive panel object sharing menus and guest dialogs outside the pane header,
  and team invite flows outside settings.
- `billing_and_referrals`: upgrade modals raised by quota or plan errors outside the workspace view.
