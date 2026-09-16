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

This is a different storage layer from conversation history. Inspection of the
installed Claude application source shows that Code sidebar groups are stored
in the renderer's `dframe-store`, under `customGroupsByScope`, keyed by
`accountUuid/orgUuid`. A scope contains:

- group IDs and names;
- assignments from keys such as `code:local_<session-id>` to group IDs;
- ordered chat keys within groups.

Claude also mirrors group scopes under
`claude_desktop_config.json` → `preferences.epitaxyPrefs` →
`dframe-group-scopes`, and has server synchronization for groups. Its renderer
keeps existing target scopes rather than unconditionally replacing them from
that mirror. The inspected live mirror is empty.

Switchboard unions `local_*.json` chat indexes, then replaces Chromium stores
with the incoming account's saved stores. It never merges the source group
definitions or assignments into the target account/organization scope. The
result is exactly the observed shape: chats arrive, their group associations
do not, and Claude places them in Ungrouped.

The local project-cloning feature in PR #5 is a separate operation. Its project
membership support does not synchronize these native Claude sidebar groups
during account switching.

**A backend-only fix is feasible, but requires a dedicated storage adapter.**
It must read and update only the relevant `dframe-store` scope while Claude is
stopped, reconcile group definitions, assignments, explicit removals, and order
using a baseline, keep the preference mirror consistent, and account for
Claude's server refresh. It must preserve the target account's other browser
state and provide rollback for both stores. Tests must cover existing empty
target scopes, A→B→C, rename/move/delete conflicts, and a renderer/server refresh.

Changing only the preference mirror would not fix existing target scopes.
Copying the whole outgoing browser store would also copy authentication and
account-specific state. Neither is a valid shortcut. This investigation does
not implement the group adapter or establish whether every previous group
definition remains recoverable in saved browser snapshots. No Switchboard UI
change is inherently required; cloud Claude Projects and their permissions
would require separate treatment from these local Code sidebar groups.

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
the intended fixes. Before calling the project-group defect fixed, implement
and verify the scoped storage adapter described above. Restoring access to the
same hosted artifact and comment monitor additionally depends on supported
Claude account permissions; silently republishing is a different operation.
