# Continue in another app

The two-step UI searches existing local chats and creates an independent destination
conversation with separate historical user/assistant messages. Continue creates the
session, verifies the stored text and roles, then opens its exact ID. It uses the
existing destination sign-in. No model request is submitted while cloning.

## Destinations

| Destination | Build 23 behavior |
| --- | --- |
| Codex Desktop | Convert selected messages into a temporary rollout; Codex app-server forks it into its own native store, names it, and reads it back. Open `codex://threads/<id>`. |
| Codex CLI | Same native creation in the shared or separately configured CLI store; launch `codex resume <id>`. |
| Claude Code CLI | Atomically publish a new linked JSONL session with original roles/text and a new session ID; launch `claude --resume <id>`. |
| Claude Desktop · Code | Create the Claude Code session, then run `claude --resume <id> /desktop` in a private pseudo-terminal for Claude's desktop handoff. This route requires the official CLI and an eligible signed-in Claude subscription. The user confirmed this handoff works in build 22; build 23 removes the visible Terminal launcher. |
| Claude Desktop · Chat | Native history import is unavailable. Continue is disabled unless the user explicitly enables context handoff under Advanced. Never silently switch to Code or label a prepared prompt as a clone. |

Codex's path-based fork is an experimental, version-sensitive protocol adapter,
verified with Codex CLI 0.151.0. It does not call the cloud-only resume-history API,
write SQLite indexes, import account configuration, or inherit running goals. The
Claude transcript writer is a compatibility adapter, not a vendor-supported import
API. Its output was independently read with the official Claude Agent SDK 0.3.267;
the installed CLI is 2.1.250. Runtime read-back rejects missing or changed messages.

The source snapshot is converted, including Codex-to-Codex routes: this build clones
visible conversation history, not tool execution state. The original remains unchanged.
A destination receipt is saved before verification. Retry rechecks the same session;
an uncertain fork without a returned ID blocks automatic duplication. A new deliberate
clone request gets a new identity. Once published, session files belong to the destination.

## Search and minimal controls

| Source | Discovery |
| --- | --- |
| Codex Desktop | Read-only local `state_*.sqlite` catalog; legacy JSONL and paginated `thread_history_1.sqlite` content |
| Codex CLI | CLI-origin sessions, including Switchboard CLI clones, in the selected shared/separate CLI store |
| Claude Code CLI | Primary JSONL histories under the configured Claude projects directory; subagents excluded |
| Claude Desktop · Chat | Cloud history is not exposed through local discovery. Export/paste stays under More options. |

The source's existing working directory is selected when available. Otherwise an
isolated workspace is created beside the clone's local receipt. Choose a different
project folder under Advanced. Filesystem contents and uncommitted changes are not
cloned with a conversation. Remote/cloud-only sessions remain outside local support.

Export/paste remains under More options. Omissions, project folder, message range,
summary, next step, selected files, transcript preview, and context-only handoff are
under Advanced. Omissions are allowed by default. Copy context stays under Advanced
on the result page. There is no destination-account picker.

Claude JSON exports, JSONL, TXT and Markdown can supply fallback sources. Plain text
without independently parsed roles remains a single historical message, not a claim
of reconstructed turns. Native catalogs are never copied into the imported library.

## Fidelity and local storage

User/assistant text and ordering are preserved. Timestamps and title are retained
where supported. Unsupported content, reasoning, tool execution, system/developer
instructions, and unresolved attachment bytes are disclosed as omissions. Original
model internals, permissions, credentials, jobs, and context-window state do not transfer.
Opening a session does not automatically resubmit its last user message.

Selected files are copied into private local storage. A supplemental history message
references those copies; they are not automatically uploaded as native multimodal
attachments. Explicit summary/next-step text becomes a supplemental user message. The
default next-step placeholder adds nothing to the cloned history.

Library, source snapshots and receipts live under
`~/Library/Application Support/Switchboard Continuations/`, with private file modes.
Native sessions live in the destination's existing session store. Completed clone
receipts and attachments remain available; deleting them can break file references,
so the clone result does not offer the old Delete prepared files action.

Limits: 100 MB source input; 10,000 messages and 5 MB transcript text; 10 selected
files and 20 MB combined bundle. The 48 KB inline-context limit applies only to
explicit context handoff, not native cloning. Claude project keys over 200 encoded
characters require choosing a shorter workspace path. No silent truncation occurs.

`CODEX_HOME` and `CLAUDE_CONFIG_DIR` are honored when supplied to the app; GUI launches
do not inherit terminal-only variables. The separate Codex CLI store is selected from
Switchboard's existing CLI mode. No credentials are copied by the cloning engine.

## Verification

`./macos/run-tests.sh` runs the existing menu/account checks, continuation checks,
and a native clone harness. The native harness uses synthetic text and isolated
stores, never a live chat or model turn. When Codex is installed it exercises actual
app-server persistence, restart/read-back, title, exact text/roles, completed history,
and receipt-based retry. Without Codex it explicitly reports the integration skip.

The build's independent Claude SDK check verifies discovery and all 40 fixture
messages, including Unicode paths and text. It calls only session readers, never
an agent query. The user confirmed the signed-in `/desktop` handoff in build 22. Build 23 tests
the hidden PTY transport with synthetic commands: real terminal descriptors, large
output, exit status, timeout/child cleanup, cancellation, and script cleanup. It
does not repeat live chat transfers. CLI destinations still open Terminal normally.
If Claude requires interaction, an explicit Open in Terminal action under Advanced
opens the same saved clone for recovery. No Terminal window is opened automatically
on error and no existing terminal is closed.

Sources: [Codex deep links](https://learn.chatgpt.com/docs/app/commands),
[Codex app server](https://learn.chatgpt.com/docs/app-server),
[Claude desktop handoff](https://code.claude.com/docs/en/desktop),
[Claude session readers](https://code.claude.com/docs/en/agent-sdk/python).
