# Chat sync between Macs

Switchboard shares local chat history through your private iCloud Drive. Codex
and Claude have separate histories. Within each app, the history is shared across
your accounts; sign-ins remain local to each Mac.

## Set up both Macs

1. Install the same Switchboard build on both Macs and enable iCloud Drive using
   the same Apple Account.
2. Open Codex and Claude on the new Mac and sign in normally. Add the accounts you
   want to use in Switchboard. Create a local Cowork chat in each Claude account
   once so Claude creates its native account/organization folders.
3. In Switchboard, open **Chat sync** and turn on **iCloud chat sync** on both Macs.
4. Finish your work and quit the app you want to sync, including its CLI sessions.
   Keep Switchboard open. It checks every minute; **Sync now** starts a check
   immediately. Reopen the app after its sync status is ready.

Switchboard does not quit apps or interrupt tasks. A running provider waits while
the other provider can sync. The first transfer also waits for iCloud to finish
downloading complete packages on the receiving Mac. To keep the archive available
offline, use Finder's **Keep Downloaded** option on its folder.

The archive is in `iCloud Drive/Switchboard/Chat Sync`. The local settings and
recovery records are in `~/Library/Application Support/Switchboard Sync/`. Turning
sync off keeps both the existing native chats and the archive.

## What is transferred

Codex transfers the native conversation catalog records, complete rollout history,
paginated history (including tools and other native item types), archive flags,
referenced projects/sections, and supported attachments in Codex's own asset
folders. It uses the configured `CODEX_HOME`, normally `~/.codex`, shared by
Desktop and the default CLI. A separately configured CLI store is independent.
Both Codex installations must have compatible native schemas. Open Codex once
on a new Mac before importing; Switchboard does not invent or downgrade schemas.
Unindexed legacy rollouts are reported rather than silently omitted; let the
official client index them before syncing that store.

Claude transfers local Cowork indexes with their native transcript trees,
subagent history, outputs, uploads, audit records, and supported project memory.
It supports full and shortened session directory names and agent sessions.
Restoration makes the native history available in existing local account scopes.
The original owner is retained as provenance, while native storage paths are
adapted for the receiving Mac. Ordinary cloud Claude chats already belong to the
Claude service; this feature does not copy or change their ownership.

The transport excludes app login stores, cookies, OAuth tokens, Keychain items,
runtime diagnostics, queued actions, and active process state. Destination
connectors authenticate locally. Chat content and files are private user data:
the sync directory is intended for your own iCloud account, not a shared/public
folder. Files intentionally included in a conversation remain part of its native
history; this is not a content-redaction service.

Project working directories, Git repositories/worktrees, arbitrary files outside
the native app stores, installed tools, and live execution environments must also
exist on the new Mac. A conversation's saved history is not a backup of the whole
computer. Historical text remains as recorded, including literal old paths.
Home-directory references in supported native metadata are relocated. If a
project has a different location, configure a local path mapping:

```sh
ai-usagebar chat-sync map-path /Users/oldname/Projects/example /Users/newname/Work/example
```

Set mappings before the first import. They apply to subsequent restores; changing
a mapping does not rewrite already-current native chats by itself.

## Updates and conflicts

Each completed revision is an immutable package, addressed by a content hash.
Macs never share a live SQLite file, a mutable cloud catalog, or credentials.
The receiving Mac validates the package and restores through the native adapter
with local recovery records. Retries reuse the saved operation rather than
blindly making another copy.

A continuation advances an unchanged local copy. If both Macs independently
continue the same chat, both versions become separate native chats. Changes are
not selected by timestamp and messages are not concatenated into a fabricated
conversation. Codex and Claude remain separate even when identifiers coincide.

Cowork also tracks the last installed version in each local account scope. A chat
continued in one account advances the older copies in the others. Independent
local edits are preserved as native forks. Differing Cowork project-memory
snapshots receive separate project identities so an older arriving chat cannot
overwrite newer shared project memory.

Archive flags are part of the transferred native state. Deleting a local chat
does not erase its retained iCloud revisions or delete it from the other Mac.
There is no automatic history pruning. Invalid or unsupported native layouts,
missing transcripts, oversized captures (512 MiB combined serialized history per
provider per pass), oversized packages (512 MiB), and conflicting external assets
stop the affected operation with a visible status; history is never silently
truncated. Adapter-specific file and session limits can be lower.

## Command line

```sh
ai-usagebar chat-sync status --json
ai-usagebar chat-sync enable --json
ai-usagebar chat-sync run --json
ai-usagebar chat-sync disable --json
```

Enabling only saves the setting. The menu app starts the periodic checks. The CLI
`run` command performs one pass and is a no-op while disabled. For synthetic
integration tests or another private file transport, `enable --folder /absolute/path`
selects an existing directory. Once a sync record exists, its transport folder
cannot be changed in place: the archive and its revision ancestry belong together.

## Verification

Focused Rust tests use synthetic native Codex databases, Cowork files, two Mac
stores, and an ordinary temporary directory standing in for iCloud. They exercise
native read-back, account sharing, concurrent edits, interruption/retry, provider
separation, unsafe paths, and credential exclusion. When an installed Codex
app-server is available, an isolated integration test creates and reads a
synthetic native conversation through it, verifying restored user, assistant,
and tool history without requesting a model turn. The macOS UI tests cover
explicit enable/disable, preview restrictions, periodic checks, and error handling.
They do not log or upload live chat content and do not imply an observed transfer
between two physical Macs.
