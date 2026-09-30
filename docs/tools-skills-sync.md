# Tools & skills

Switchboard shares selected personal skill folders and MCP setups through
`iCloud Drive/Switchboard/Tools & Skills`. Automatic chat sync is retired; [manual chat transfers](mac-transfers.md) are separate. Definitions are
shared across accounts; each app and account keeps its own authentication.

## Set up both Macs

1. Install the current Switchboard build on both Macs and sign into the same
   iCloud Drive account. Keep the Switchboard folder downloaded in Finder.
2. Open **Tools & skills**. Enable **Sync skills**, **Sync MCP setups**, or both.
3. In **Available**, select the personal items to adopt and their destinations:
   **Codex**, **Claude Code**, and/or **Cowork**. Opening the page only inventories
   files; it does not upload your whole configuration.
4. Close the relevant Codex or Claude clients when ready, then use **Sync now**.
   While Switchboard runs, it retries approximately every minute and pauses
   around account operations. Running clients are left alone.
5. On the second Mac, enable the same categories. Selected items install in the
   supported local destinations once the apps are closed and iCloud has delivered
   complete revisions. Install required runtimes and sign into services locally.

Codex uses the personal `~/.agents/skills` root, existing legacy skills under
configured Codex homes, and MCP entries in `config.toml`. The default home,
`CODEX_HOME`, and an existing Switchboard separate CLI home are included. Claude
Code uses `skills` and user MCP settings for the default home, `CLAUDE_CONFIG_DIR`,
and account homes configured in Switchboard. Project-local settings are outside
this feature. Already installed plugin and account-managed caches stay under
their original updater, including Claude's `skills/synced` cache.

## Cowork

Cowork loads account plugins; it does not discover Claude Code's personal skill
folder. Use **Install/update in Cowork** to export a plugin ZIP, reveal it in
Finder, and open Claude. In Claude, go to **Customize → Plugins → Add plugin** and
upload the ZIP through the custom-plugin flow. Repeat for each Claude account.

Switchboard tracks the exported version and your explicit import confirmation
separately. Confirmation is not a verified account installation. It does not
silently upload or update plugins. When the library changes, export and import
the replacement. After removing the final item, import the empty replacement
to remove the old plugin content.

Only compatible skills and portable remote MCP definitions can be exported.
Local MCP executables are excluded because Cowork has a separate execution
environment. An incompatible selected item blocks replacement export and keeps
the previous ZIP; review the item or remove its Cowork destination. Skill scripts
and assets are included, but their dependencies must exist in Cowork.

## Conflicts, removal and setup

Edits from either Mac create immutable revisions. Concurrent edits keep both
candidates and retain the last installed content until you choose a version.
Revision hashes identify the choices; the corresponding JSON packages are in
the private iCloud library when a detailed comparison is needed.

Turning a category off pauses synchronization and keeps installations. Disabling
or removing an individual item publishes an explicit revision. Switchboard
removes only unchanged files or MCP entries it owns. A locally edited item is
preserved and needs review. A missing local file alone does not delete the shared
item. Unmanaged name collisions are never overwritten.

MCP setup synchronization does not establish an authenticated connection. Remote
MCP entries remain **Sign-in needed** until you verify them in their client; sync
does not probe servers. Embedded environment/header values, private URLs,
machine-specific paths, and ambiguous arguments remain local. These slots may
need setup on the other Mac. Unknown client options are not translated silently.
An existing native Codex `enabled = false` remains disabled on that Mac.

For advanced local bindings, the bundled backend supports environment references
and existing local paths. Values themselves are never command arguments here:

```sh
BACKEND=/Applications/Switchboard.app/Contents/Resources/bin/ai-usagebar
"$BACKEND" library-sync inventory --json
"$BACKEND" library-sync status --json
"$BACKEND" library-sync bind ITEM_ID --target codex --slot env:API_KEY --env-var MY_API_KEY --json
"$BACKEND" library-sync bind ITEM_ID --target claude-code --slot argument:0 --path /absolute/path/server.js --json
"$BACKEND" library-sync run --json
```

Environment references must be available to the Switchboard backend. Existing
destination-native credentials take precedence. The binding command applies to
configured homes of the selected app; configure an account directly in its native
client when it needs a distinct value. Setup requirements expose slot names, not
secret values or source file contents. Bindings belong to the selected library
item, destination configuration and slot, so different MCP servers cannot share
or overwrite one another's bindings.

Older destination-only bindings are kept locally but are not assigned to a
server automatically. Existing values in that server's native configuration are
preserved. If a required value is missing, the destination reports **Needs setup**;
run `library-sync bind` again for that item and slot, then retry sync.

Local settings, receipts, ownership records, recovery journals and Cowork ZIPs
live under `~/Library/Application Support/Switchboard Library`, outside iCloud.
Recovery journals may contain private native configuration and must not be
published. iCloud skill packages contain complete selected folders: review their
content before adopting them. Known credential files such as `.env` block the
folder's adoption; this is not a detector for every secret embedded in arbitrary
source text. Symlinked roots/files, escaping paths, and special
files are left untouched. This first format limits each skill to 20 MiB, each
revision to 32 MiB, and archive scans to 128 MiB/10,000 revisions. It does not prune
history automatically.

## Verification scope

Automated checks use synthetic two-Mac and multi-home fixtures for convergence,
conflicts, account separation, interrupted writes, ownership, credentials and
native configuration preservation. Swift checks cover opt-in controls, preview
guards and the guided Cowork flow. Physical transfer between two user Macs and
live authenticated MCP use depend on the user's devices and service sign-ins;
automated fixture checks do not establish those results.

The build-33 fixture was read successfully by Codex CLI 0.151.0 and Claude Code
2.1.250. The official Claude plugin validator accepted its exported manifest,
skills, assets and MCP configuration. The placeholder MCP endpoint was not
connected. Native Cowork account upload remains unverified: the installed app's
Customize/Plugins controls were visible, but UI automation could not open its
upload menu reliably.
