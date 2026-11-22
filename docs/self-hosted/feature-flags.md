# Feature flags

neo-warp turns whole product areas on or off with a flag file. The app reads the file itself at
startup; no flag service runs. The file uses the [flagd](https://flagd.dev) flag-definition
format, so a flagd server could serve the same file later.

## Where the file goes

Put `flags.flagd.json` next to `organization.toml`:

| OS | Path |
| --- | --- |
| macOS | `/Library/Application Support/Warp/flags.flagd.json` |
| Windows | `%ProgramData%\Warp\flags.flagd.json` |
| Linux | `/etc/warp/flags.flagd.json` |

The file is optional. Without it, the defaults in the table below apply. Flags take effect when
the app starts, so people must restart neo-warp after you change the file. The app reads flags only
when `organization.toml` configures organization sign-in.

## Format

```json
{
  "$schema": "https://flagd.dev/schema/v0/flags.json",
  "flags": {
    "warp_drive": {
      "state": "ENABLED",
      "variants": { "on": true, "off": false },
      "defaultVariant": "on"
    }
  }
}
```

- `defaultVariant` picks the value. Set it to `"on"` or `"off"`.
- List only the flags you want to change. Flags you leave out keep their default.
- `"state": "DISABLED"` also keeps the default.
- `targeting` rules are not evaluated. A flag with targeting uses its `defaultVariant`.
- If the file isn't valid JSON or doesn't match the format, neo-warp ignores the whole file, uses
  the defaults, and writes an error to its log. Unknown flag names are ignored with a warning.

## Flags

| Flag | Default | When off |
| --- | --- | --- |
| `warp_drive` | off | Hides Warp Drive and its objects: workflows, notebooks, env var collections, and Drive-backed rules |
| `settings_sync` | off | Settings stay on the device; the settings sync toggle is hidden |
| `cloud_agents` | off | Hides cloud (Oz, ambient, scheduled) agents, runners, cloud environments, agent API keys, and handoff |
| `warp_hosted_mcp` | off | Hides Warp-hosted MCP servers and the MCP gallery. Local MCP servers still work |
| `sharing_and_teams` | off | Hides block and session sharing, guests, teams, and invites |
| `billing_and_referrals` | off | Hides billing, credits, referrals, and upgrade prompts |
| `block_titles` | off | No AI-generated titles for shared blocks |
| `conversation_cloud_sync` | off | Agent conversations stay on the device. Local history still works |
| `voice_input` | on | Hides voice input |
| `codebase_indexing` | on | Hides codebase indexing |
| `ai_prompt_suggestions` | on | Hides AI prompt suggestions |
| `ai_command_search` | on | Hides AI command search |

The areas that default to off need Warp's own cloud service, which neo-warp doesn't use. Turn one
on only if your deployment provides that service.

## Testing a file

In development builds, set `WARP_ORG_FLAGS_FILE` to a flag file to override the system file.
Shipped builds ignore this variable. The log lists the value of every flag at startup
(`Organization flag ... is on|off`).
