# Automatic chat sync (retired)

Automatic iCloud chat sync was removed in build 35. Switchboard no longer scans,
publishes or restores whole chat stores on a schedule. The Chat sync page and its
startup/timer checks are gone, and legacy `enable`, `run` and `map-path` commands
return a retired-feature error. A previously enabled setting is inert in build 35.

Use [manual Mac transfers](mac-transfers.md) to export selected conversations and
choose their destination app and local project folder on the receiving Mac.
This is a transcript transfer, not native database replication. Its supported
sources and destinations differ from the former Codex/Cowork sync feature.

## Existing data

Existing chats, `iCloud Drive/Switchboard/Chat Sync` archives and local settings and
recovery records under `~/Library/Application Support/Switchboard Sync/` are kept.
The update does not delete them, rewrite native histories, or import old archives.
Do not delete recovery data merely because automatic sync has been removed.

The retained diagnostic commands do not read or upload native conversations:

```sh
ai-usagebar chat-sync status --json
ai-usagebar chat-sync disable --json
```

Status reports `enabled: false`, `retired: true` and the stored `legacy_enabled`
value. Disable clears that legacy flag without changing archive paths, mappings,
native chats or recovery records. An older Switchboard build can still honor its
old enabled setting, so disable it before rolling back or replace it on every Mac.

Personal skills/MCP library sync and local account switching are separate features.
They retain their existing behavior. Credentials are not included in chat transfers.
