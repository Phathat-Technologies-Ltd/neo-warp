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
4. Builds that are not organization builds ignore every flag file and keep upstream behavior.
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
| `block_titles` | off | AI-generated titles for shared blocks |
| `conversation_cloud_sync` | off | Cloud conversation history and sharing. Local history stays |
| `voice_input` | on | Voice input |
| `codebase_indexing` | on | Codebase indexing |
| `ai_prompt_suggestions` | on | AI prompt suggestions |
| `ai_command_search` | on | AI command search |

When a flag is off, the entry points for its feature are hidden and the client stops calling the
server for it.
