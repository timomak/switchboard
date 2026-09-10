# Continue in another app

Switchboard can prepare a portable conversation for Claude Desktop Chat,
Claude Code CLI, Codex Desktop, or Codex CLI. Open **Continue in another app…**
from the account popover, search an existing local chat, choose an app,
and select **Prepare**. **Copy context** copies the prepared text;
**Open** launches the destination using its existing sign-in. Paste the context
into a new conversation. Add files from **Details → Show files** when needed.
For a CLI, select a project folder under Advanced before opening it.

This creates a new-conversation handoff. It does not create native history entries,
submit prompts, auto-paste, move running work, switch accounts, or call model APIs.
The ready state means files are prepared locally. The destination remains responsible
for sign-in, submission, permissions and model behavior. No public native-thread
creation/deep-link interface is assumed.

## Sources and search

Search reads the selected source directly. The list stays in memory and the selected
transcript is loaded when you choose **Next**. **More options → Refresh chats**
reloads the catalog. Existing sign-ins are unchanged.

| Source | Direct discovery |
| --- | --- |
| Codex Desktop | Local `state_*.sqlite` catalog; legacy rollout JSONL and paginated `thread_history_1.sqlite` transcripts |
| Codex CLI | CLI-origin sessions in the same catalog; session files when no catalog exists |
| Claude Code CLI | Primary JSONL sessions under `~/.claude/projects`; subagent folders and sidechains excluded |
| Claude Desktop Chat | Cloud chats are not available through local discovery; export/paste remains a fallback |

`CODEX_HOME` and `CLAUDE_CONFIG_DIR` override the default roots when present in
Switchboard's environment. Launching a GUI from Finder does not inherit terminal-only
environment variables. Codex records whose transcript files are gone remain visible
as **Not stored locally**, with selection disabled. Unknown database schemas produce
a recovery message. Catalogs are opened read-only; credentials and cookies are not read.

Exports and pasted text remain under **More options**. Claude `conversations.json`,
primary-session JSONL, TXT and Markdown are supported. ZIPs are not. Malformed or
changing session files fail rather than yielding silently truncated context. Import
batches are all-or-nothing; native catalogs are never saved into the imported library.

Claude Code and current Codex also have a vendor-native import route. This feature
does not wrap that experimental protocol: use Codex Settings → Import or CLI
`/import` directly when appropriate. See the [official import guide](https://learn.chatgpt.com/docs/import).

## What transfers

Messages, code text, available timestamps, an optional user-written summary,
a next step, and explicitly chosen files. Source paths are not followed from a
transcript; referenced attachments require the user to add their originals.
Unsupported blocks, missing attachment content, tool activity and excluded earlier
messages are disclosed. Attachments/unsupported blocks require an explicit decision
before preparing. General metadata/tool omissions stay under Advanced.
The manifest and context both describe omissions. No hidden reasoning, credentials,
source tool permissions, native IDs or running process state is restored.

**Advanced** contains summary, next step, project folder, starting message, selected
files and the complete context preview. An overly large context blocks Prepare.
The user can select a later starting message; nothing is silently truncated.

## Storage and privacy

Library and bundles live in `~/Library/Application Support/Switchboard Continuations/`.
They are private local files (directory mode 0700, file mode 0600), not encrypted.
No conversation telemetry or provider upload occurs. **Remove imported copy** removes
that library entry; it does not change the original export. **Show local files…**
opens the library location so stored bundles can be managed. **Delete prepared files**
removes the current bundle. Successfully prepared bundles persist until explicitly
removed, so a destination's file reference is not broken by an expiry timer.
Cancellation or failed preparation removes the staging directory. Cancellation
never kills source work or claims to undo a submitted destination message.

Limits: 100 MB total selected input/library, 200 stored chats, 10,000 messages and
5 MB of text per chat; 48,000 UTF-8 bytes of prepared context; 10 selected files and
20 MB total context/file bytes. These are Switchboard limits, not provider context
or upload guarantees. Binary file acceptance must still be checked by the destination.
Files are regular-file-only, bounded reads with no final-component symlinks; changed
files fail preparation. Generated output names cannot traverse paths or overwrite
receipts. Source messages can contain instructions; the wrapper treats them as
historical reference and never executes any of their contents.

## Verification

`./macos/run-tests.sh` runs the existing menu/account harness and isolated continuation
checks. The latter use synthetic JSON, SQLite databases, text and temporary files only. They cover
ordering, duplicate identity, malformed input, Codex event/response duplication,
missing attachments, bounded reads, symlinks, permissions, source preservation,
receipt identity, changed-file detection, range disclosure, cancellation and cleanup.
`./macos/build.sh` and `./macos/bundle.sh` compile/package the native app. No app
installation, account switching or live chat transfer is part of these checks.

A fixture-only native UI harness can be built with `-D SWIFT_TEST_HARNESS
-D CONTINUATION_PREVIEW`, compiling the app, account-switchboard, continuation-core,
continuation-discovery, continuation-ui and continuation-preview Swift files. Pass an isolated output directory.
It suppresses clipboard writes and destination launches. Do not run the normal app
entry point for fixture verification.
