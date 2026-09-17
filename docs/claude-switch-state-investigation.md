# Claude account-switch state investigation

Investigated on 2026-09-16 against main at `260172d`, the installed Switchboard
build 31, and Claude Desktop 1.52386.6. Account stores were read only for scoped,
sanitized metadata checks; regression fixtures contain synthetic data. No live
switch, account repair, publishing, or app installation was performed.

## Findings by symptom

### Artifact disappears and its comment monitor cannot resume

[PR #6](https://github.com/timomak/switchboard/pull/6) was merged, but the running
installed helper lacks the preservation implementation. The expected backup
directory is absent. Merge status did not establish deployment to the installed
app.

The incident's local artifact source still exists. The screenshot establishes
failure to restart its comment monitor, not deletion of that source. Local
source snapshots protect recoverable content; they do not transfer ownership,
permissions, or comment-monitor authorization for an artifact hosted by Claude.
The exact server-side reason for this monitor's failed restart was not observed.

There is also a format defect in PR #6 itself: the incident's native frame URLs
use `/artifact/<22-character token>`, while the preservation code accepted only
`/code/artifact/<UUID>`. The follow-up accepts both validated URL forms. Native
frame records were present; the backend artifact ID shown in the notice differs
from the URL's opaque sharing token.

See [the artifact investigation](claude-artifact-investigation.md) for the
format-specific findings and the distinction between source preservation and
hosted artifact continuity.

Follow-up on 2026-09-17: the bundled Claude Code CLI has a fixed table of
reasons a comment monitor cannot resume, each with its own wording —
`other_org` ("in another of your organizations"), `not_editor`,
`held_by_live_session`, `held_by_job`, `watch_cap`, `stop_latched`,
`recorded_stop`, `record_incomplete`, `auto_replies_disabled`,
`stale_handoff`, `comments_unavailable`, `holder_unknown`, `arm_in_flight`.
The notice in the screenshot ("it couldn't restart in this session") is the
default branch, reached when the artifact's boot request fails outright
(`boot_failed` / `not_found` / no subscription token) rather than by any
recognised policy. That is the expected outcome for an artifact published by a
different *account*: the resumed session is signed in as tmakhlay2 and has no
access to it. Nothing local can supply that access; the recovery is to open
the conversation under the publishing account (the monitor re-arms there) or
to publish the preserved source again under the new account, which creates a
new artifact and URL.

### A paused routine becomes enabled

Three independent defects interact:

1. **The baseline loses organization scope.** Schedule files live under
   account/organization directories, but the old sync record stores just one
   task definition per account. Reading a second organization overwrites the
   first baseline. On the next merge, an unchanged stale definition can appear
   to be a new edit. Each of the three inspected accounts has two organization
   registries, so this is relevant to the reported sequence.
2. **Whole-task comparison confuses execution bookkeeping with user edits.** A
   pause in one copy and a changed `lastRunAt` in another become a conflict
   between entire task objects. Keeping the target object can keep its old
   `enabled: true`, discarding the pause. A synthetic regression reproduced this
   exact failure before the fix.
3. **Planning precedes shutdown.** The old implementation renders the target
   schedule while Claude is running and writes those bytes after quit. A pause
   flushed during shutdown can be overwritten with the pre-quit state.

The scoped inspection found two shared routines with differing enabled states.
The old sync record already contained the disagreement and had no reconciled
canonical definition for those routines. It cannot establish which historical
action first caused each divergence; the structural defects and the divergence
are confirmed independently.

The backend remedy is to retain organization-specific baselines, reconcile
independent field edits, track enabled state separately from unrelated task
conflicts, and recompute the switch plan after Claude finishes quitting.
Existing ambiguous enabled/paused copies need a conservative paused fallback;
subsequent explicit resume actions must propagate through the new baseline.

The old schedule writer also reconstructs only `scheduledTasks` and
`recordedSkips`, dropping other target registry fields such as retry state and
migration markers. The fix preserves the target document's remaining fields.

### Projects disappear and chats move to Ungrouped

Follow-up on 2026-09-17, reading every saved browser snapshot with a LevelDB
reader (the `.ldb` tables are Snappy-compressed, so plain string searches of
the earlier pass were not evidence of absence).

The Code sidebar is the renderer's `dframe-store` `localStorage` entry, a
zustand store persisted per browser profile — that is, per Switchboard
account snapshot. Custom groups (shown as projects) live in
`customGroupsByScope["<account>/<org>"]` as `{groups, assignments, order}`;
the grouping and sorting mode live in top-level `groupByByMode` /
`sortByByMode`. Assignment keys are `code:local_<session-id>`.

What the snapshots hold:

- **gamers.cccp** (outgoing before the incident): `groupByByMode.code =
  "project"` — the sidebar was grouped by *folder*, which the app derives from
  each chat's working directory. Its scope has no custom groups. This is what
  the user saw as their projects.
- **tmakhlay2** (incoming): `groupByByMode.code = "custom"` with four custom
  group definitions and **no assignments**, in every snapshot that has this
  scope — including the archive taken on 2026-09-08 before the first switch.
  Its last rendered row counts show all recent chats under `custom-ungrouped`
  and zero rows per group.

So nothing was lost at the incident switch. The incoming account's saved store
brought back its own view mode ("custom") together with four long-empty groups,
while the outgoing account's folder grouping is a per-account preference that
was not carried. Every merged chat therefore landed under Ungrouped. No
assignment of a local chat to a custom group exists in any local snapshot; if
those four groups ever had members, it predates the earliest snapshot and is
not recoverable from local data.

Two app behaviours matter for a fix:

- Group definitions and the view fields are synced to the server per
  organisation (`/api/claude_code/organizations/<org>/user_settings`, entry
  `ccd/dframe-store`). On reconcile after launch, a differing server copy
  replaces local group definitions and drops local assignments whose group is
  not on the server. A local edit survives only when the app's own marker
  `ccd-sync-pending:ccd/dframe-store` holds the current `<account>/<org>`
  identity, in which case the local state is pushed instead. Assignments of
  `code:local_*` chats are never uploaded.
- The main process mirrors non-empty scopes to `claude_desktop_config.json` →
  `preferences.epitaxyPrefs.dframe-group-scopes` and merges that mirror into
  the store on hydration (adopting absent scopes wholesale; for present scopes
  only assignments to existing groups).

**Implemented** on `codex/claude-sidebar-tombstones`: a LevelDB adapter for
the renderer's `localStorage` (pure Rust, staged copy, read-back verification,
swap into place), a baseline-driven reconciliation of the sidebar across
accounts (groups persist until deleted; a total wipe is not trusted as a
deletion), the pending-edit marker so the server keeps the merged result, the
mirror kept consistent, and sync-record fields for the baselines. Stores
written this way were read back by upstream LevelDB (Node `classic-level`)
with every original entry intact. Cloud Claude Projects are unrelated to these
local sidebar groups.

### Archived chats become unarchived

Claude's installed `archiveSession` and `unarchiveSession` implementations save
`isArchived` without updating `lastActivityAt`. Switchboard previously selected
the entire chat index using only that activity timestamp. An archive edit at
the same timestamp is ignored; a newer resume record can also overwrite an
archive flag with an older value.

The inspected stores contain three shared chats with differing archive flags
at identical activity timestamps. This directly matches the algorithm's blind
spot. Earlier deletion reconciliation tracks a missing index, so it does not
handle an archive toggle on an index that still exists.

The backend remedy is to reconcile archive state independently from resume
content, retaining organization-specific observations and a reconciled archive
baseline. The newest resume data must survive while a known archive or
unarchive edit follows A→B→C. Without historical evidence, conflicting copies
should remain archived until an explicit unarchive establishes new intent.

The app also keeps a per-folder `archived-sessions.idx` load hint
(`{"v":1,"archived":[ids]}`) used to defer loading archived records. The
follow-up regenerates it from the reconciled flags wherever it exists.

### Deleted chats return

Claude Desktop records every user deletion as a `deleted_<id>` marker file
(content: deletion time in ms) beside the account/org session indexes, for the
session's local id and each CLI transcript id it owned, and consults the
signed-in account's markers before re-adopting a transcript from
`~/.claude/projects/`. The inspected store holds 44 markers; 12 chats with a
marker in one account still have index copies in all four org folders — the
history merge handed them back, and the app's marker check does not cover
another account's folder.

**Implemented**: markers are the recorded intent, so a switch removes the index
from every account without a prompt and copies the marker into every
account/org folder. An index created after the marker's time (a deliberate
re-import) supersedes it and is kept. The existing prompt remains for indexes
that vanished without a marker.

## Delivery boundary

The follow-up changes live on `codex/claude-switch-state`, based on current main,
in a separate worktree. They preserve Switchboard's existing interface and
account-switch workflow. Synthetic tests cover state reconciliation and changes
flushed on shutdown. They do not prove cloud artifact authorization or native
group synchronization, and they do not repair the installed app or existing
account data by themselves.

Verification completed:

- `cargo test --locked --lib claude_desktop:: -- --test-threads=4`: 104 passed.
- `cargo test --locked --lib account::tests -- --test-threads=4`: 39 passed.
- `cargo clippy --all-targets --locked -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.
- Independent source review of the switch lifecycle and state reconciliation
  found no additional actionable correctness issue.

The synthetic coverage includes two consecutive switches, archive and pause
preservation, newer resume content, later explicit unarchive/resume, ambiguous
legacy organization baselines, state flushed during quit, stale deletion
confirmation, capture seeding, and the observed native artifact URL format.

Before calling a future release installed, verify its bundled helper contains
the intended fixes. Restoring access to the same hosted artifact and comment
monitor additionally depends on supported Claude account permissions; silently
republishing is a different operation.

The sidebar and deleted-chat follow-up on `codex/claude-sidebar-tombstones`
adds the `rusty-leveldb` crate (pure Rust) and is covered by synthetic tests
for the LevelDB round trip, the reconciliation rules, marker handling, and the
load hint. It has not yet been exercised against a live Claude launch; the
first real switch should be followed by confirming that the sidebar shows the
carried grouping mode and that the app's server sync keeps it (the
`ccd-sync-pending:ccd/dframe-store` marker clears once pushed).
