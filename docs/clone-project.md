# Clone project

“Clone project…” is a companion to “Continue in another app…”. Choose a local
folder project, then a destination. The second step contains the destination, a footer Back button and
collapsed Advanced options. Name, folder mode, chat exclusions and omission details
stay under Advanced; the primary action carries the selected chat count.

Codex Desktop discovery reads explicit local project assignments and project names
from its version-sensitive local metadata. Assigned chats stay in their project even
when their working folder differs. Archived chats are excluded by default; Advanced
can include them. Explicit projectless/non-local assignments are not grouped by folder.
Other sources and older metadata schemas fall back to recorded working directories.
All discovered active chats in the project are selected initially. This does not
reconstruct destination project registries or cloud projects.
Search matches project names, paths and catalog chat titles. Advanced lets you
exclude individual chats. Source catalogs are read only.

Each readable conversation becomes a separate native chat using the existing
version-sensitive adapters. Codex Desktop/CLI and Claude Code CLI persistence use
the existing engine. Claude Desktop Code copies are initially Claude Code sessions;
the batch then performs each desktop handoff in sequence. A durable handoff marker
prevents automatic repetition after success or uncertain opening. Open remains an
explicit recovery action. Desktop project grouping is not recreated. Claude Chat
is not offered as a native destination; use the conversation tool for explicit
context handoff. Codex Desktop defaults to Claude Desktop Code.

The default is a new empty folder, stored with the batch under Switchboard's
Application Support directory. Advanced also offers:

- **Copy source files:** bounded regular-file snapshot, including untracked files.
  Excludes dotfiles, Git metadata, common generated/dependency folders, agent
  instruction files, symlinks and common secret filenames/extensions. Filename
  filtering cannot identify every credential. This is explicit opt-in. No setup
  scripts run. File modes are private; executable permissions, Git history and
  historic absolute links are not reconstructed. Copy failure aborts preparation
  before native chat creation. Files are checked for concurrent writes individually;
  the folder is not a point-in-time filesystem snapshot.
- **Use source folder:** independent chat IDs share existing working files and
  destination-discovered configuration. Later edits can affect the source project.

Visible user/assistant text is retained by the existing conversion. Project
instructions, native attachment bytes, tools and running work are omitted.
Unavailable/invalid/changing chat inputs remain in the total as omitted entries.
Results always report “N of M chats cloned”, not a complete project clone.

## Recovery

The batch manifest includes frozen readable chat snapshots, destination store,
workspace, child operation IDs, results and omissions. Each child reuses its existing
native receipt on retry. Known successful IDs are retained; an uncertain native
creation without an ID remains blocked by the native engine's pending marker.
Independent failures do not discard successful copies. Advanced → Previous copies
reopens saved batches after restart. A different destination store blocks resume.

Open is separate from creation, targets one exact chat, and uses the destination's
existing sign-in. No account picker, automatic account switch or model turn is added.
Desktop opening errors do not recreate conversations. Previous copies and their
files are retained; do not remove a batch directory while chats reference it.

Limits: 200 selected chats, 40 MB aggregate source text, existing per-chat limits,
100 MB copied regular-file bytes and 10,000 enumerated filesystem entries. No silent
truncation. The global folder-project catalog inherits the existing local discovery
limits and cannot claim complete cloud/archive membership.

`macos/run-tests.sh` includes synthetic isolated batch tests for folder boundaries,
copy exclusions, unavailable inputs, partial failure, restart/retry identities and
uncertain creation. No live destination histories or desktop UI are needed.
