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
| `codebase_indexing` | on | Codebase indexing and embedding, including the embedded server's index |
| `ai_prompt_suggestions` | on | AI prompt suggestions |
| `ai_command_search` | on | AI command search |

A flag that is off is a hard override: the product-area flag and the upstream flags that drive its
feature stay off for the whole process. Server experiments, user preferences, and runtime overrides
cannot turn them back on.

### What an off flag hides today

- `warp_drive`: the Warp Drive panel, its settings page, Drive objects as AI context, and every
  workflow, notebook, and env var collection entry point outside the panel: workspace handlers
  that open workflow, notebook, and env var collection panes, the new workflow modal, "View in
  Warp Drive", the AI workflow info box and Warp AI transcript "Save as workflow" buttons.
- `settings_sync`: cloud settings sync and its settings controls.
- `cloud_agents`: cloud mode, ambient and scheduled agents, handoff (including the Warp Agent
  settings cloud handoff toggle), cloud environments, runners, the cloud agent capacity modal,
  their settings pages, and the "Open Settings: Environments" command palette entry.
- `warp_hosted_mcp`: Warp-hosted MCP servers in the MCP gallery.
- `sharing_and_teams`: session sharing, block sharing from the terminal, the shared blocks and
  teams settings pages, and the pane header share button with its "Share pane" command palette
  entry and keybinding (Drive objects, sessions, and conversations), Drive panel object "Share"
  menu items and the object sharing dialog (including invitee-request deep links), and the
  "Open Settings: Teams" and "Open Settings: Shared Blocks" command palette entries.
- `billing_and_referrals`: billing, usage, and referrals settings pages, the workspace upgrade
  prompts gated on the flag, the resource center "Invite a friend" button, the `ShowUpgrade`
  action from any source, the shared objects tier-limit modal, the free AI removal modal, the
  pricing promotion banners, and the "Open Settings: Billing and usage" and "Open Settings:
  Referrals" command palette entries.
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

Remaining entry points that are not hidden when their flag is off:

- `billing_and_referrals`: inline "Upgrade" hyperlinks inside quota and plan error text (AI
  prompt alerts, out-of-credits replies, command search and workflow error toasts, agent profile
  and Warp Agent settings copy). They are part of server-driven error strings rendered at many
  sites; an organization server does not return these plan errors, so they do not appear in
  practice. The onboarding offer and AI access slides keep their upgrade choice because the
  onboarding crate does not depend on product-area flags and its step sequence would need
  reworking.
