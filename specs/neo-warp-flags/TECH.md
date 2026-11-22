# neo-warp feature flags: technical design

## Evaluator

A new crate, `crates/org_flags`, parses the flagd flag-definition JSON and evaluates its static
subset: `state`, `variants`, and `defaultVariant`. The Rust OpenFeature flagd provider
(`open-feature-flagd`) brings gRPC (tonic), a sync client, and a JSONLogic engine even for its
in-process file mode. For static boolean flags a small evaluator is cheaper, and the file format
stays flagd-compatible so a real flagd can serve the same file later.

Rules:
- `state: "DISABLED"` yields no value from that source.
- `targeting` is not evaluated. A flag with targeting resolves to `defaultVariant` and logs a
  warning.
- A missing `defaultVariant`, a default variant that is not in `variants`, or a non-boolean value
  yields no value and logs a warning.
- `$schema`, `$evaluators`, and `metadata` are accepted and ignored.

`OrgFlags::load` layers the sources flag by flag: the defaults compiled into the binary from
`crates/org_flags/resources/default.flagd.json` (so they always ship with the app), the admin file
next to `organization.toml` (`org_flags::admin_flags_path`), and, when overrides are allowed
(debug builds), the file in `WARP_ORG_FLAGS_FILE`. It returns the boolean value of each known
product area (`ProductArea`).

## FeatureFlag mapping

New `FeatureFlag` variants, one per product area: `WarpDrive`, `SettingsSync`, `CloudAgents`,
`WarpHostedMcp`, `SharingAndTeams`, `BillingAndReferrals`, `BlockTitles`, `ConversationCloudSync`,
`VoiceInput`, `CodebaseIndexing`, `AiPromptSuggestions`, `AiCommandSearch`. They are listed in
`PRODUCT_AREA_FLAGS`, which `init_feature_flags` enables in every build, so builds without an
organization keep upstream behavior.

`app/src/auth/organization.rs::init` is the single startup path for the GUI and the TUI (both go
through `run_internal`). When organization sign-in is configured it loads the flags and calls
`apply_org_flags`, which sets each product-area flag and, when an area is off, also turns off the
upstream flags that already gate parts of it:

- `cloud_agents`: `CloudMode*`, `AmbientAgents*`, `ScheduledAmbientAgents`, `CloudEnvironments`,
  `CreateEnvironmentSlashCommand`, `Oz*`, `Handoff*`, `PeriodicHandoffCheckpoints`, runners.
- `sharing_and_teams`: `CreatingSharedSessions`, `ViewingSharedSessions`, `SessionSharingAcls`.
- `block_titles`: `SharedBlockTitleGeneration`.
- `conversation_cloud_sync`: `CloudConversations`.
- `warp_hosted_mcp`: `FactoryMcp`, `PreviewUrlsMcp`, `WellKnownMcpIds`,
  `ManagedMcpCredentialRefresh`.

## Gates on the new flags

- Settings navigation: `SettingsSection::feature_flag` maps pages to areas (Billing, Referrals,
  Shared blocks, Teams, Warp Drive, Environments, API keys, Code indexing). Pages whose flag is off
  are removed from the sidebar.
- Warp Drive: `WarpDriveSettings::is_warp_drive_available` requires `WarpDrive`. The left panel,
  command palette, command search, keybindings (`ENABLE_WARP_DRIVE` context), AI context menu,
  and warpctrl surface already check it. The left-panel tab list and the Appearance tools-panel
  toggle check it too.
- Settings sync: the cloud preferences syncer is inert, the Account page toggle and its palette
  binding are hidden, and privacy settings stop calling `UpdateUserSettings`.
- Warp-hosted MCP: the MCP gallery ignores server updates, so it stays empty in the GUI and TUI.
- Billing and referrals: the Account page "Earn rewards" widget is hidden.

Voice input, codebase indexing, prompt suggestions, and AI command search get flags and plumbing
here; the teams that own those features gate them.

## Testing

`crates/org_flags/src/lib_tests.rs` covers parsing, `DISABLED`, targeting fallback, layering,
unknown names, invalid files, and the bundled defaults. The app-side mapping calls `set_enabled`,
which panics in unit tests, so it is kept thin.
