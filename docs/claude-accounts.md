# Claude account guide

ai-usagebar can report several Claude accounts at once. On macOS, it can also
switch the active login used by Claude Desktop and the `claude` CLI.

## Choose a setup

| Need | Recommended setup |
|---|---|
| One Claude account | Use the default Claude Code login. No extra config is needed. |
| A few named accounts | Run `ai-usagebar account add <label>`. |
| Accounts already organized by `CLAUDE_CONFIG_DIR` | Set `[anthropic] accounts_dir`. |
| Separate Waybar modules backed by files you already manage | Use `--creds-path` and `--cache-dir`. |
| Switch Claude Desktop or the CLI on macOS | Use `ai-usagebar account switch`. |

## Add a named account

```bash
ai-usagebar account add work
```

The command:

- adds a `[[anthropic.accounts]]` entry without disturbing comments or
  formatting;
- creates a credentials directory for the account;
- runs `claude` with that account's own `CLAUDE_CONFIG_DIR`.

The login goes straight to the source ai-usagebar reads: a scoped Keychain item
on macOS or `.credentials.json` on Linux and Windows. The default Claude login
is left alone. Re-run the command to sign in again, or pass `--no-login` to
register the account without opening Claude.

Once signed in, the account appears in the TUI and native integrations without
a restart, provided `[anthropic]` is enabled.

### Configure accounts by hand

```toml
[anthropic]
# Optional default account. Without this, ai-usagebar uses the platform default.
# credentials_path = "~/.claude/.credentials.json"

[[anthropic.accounts]]
label = "work"
credentials_path = "~/.config/ai-usagebar/accounts/work/.credentials.json"

[[anthropic.accounts]]
label = "personal"
credentials_path = "~/.config/ai-usagebar/accounts/personal/.credentials.json"
```

Select one with `--account`:

```bash
ai-usagebar --vendor anthropic --account work
```

Or use it in Waybar:

```jsonc
"custom/claude-work": {
    "exec": "ai-usagebar --vendor anthropic --account work --format 'w {session_pct}% · {session_reset}'",
    "return-type": "json",
    "interval": 300,
    "tooltip": true
}
```

Each named account gets its own cache under
`~/.cache/ai-usagebar/anthropic/<label>/`. The default account keeps the
original `~/.cache/ai-usagebar/anthropic/` path.

For Claude, `--account` cannot be combined with `--creds-path`. (OpenRouter
also supports `--account` through its own account array.) An unknown label
fails with a list of valid labels. The TUI shows the default Claude tab
followed by one tab for each named account.

If a CLI account and a saved Desktop profile share a label, aggregate views
such as the TUI and `usage --json` use the Desktop profile to avoid refreshing
the same rotating token from two stores. Direct widget commands are explicit:
add `--desktop` alongside `--account` when you want the Desktop profile.

## Discover accounts from a directory

Point `accounts_dir` at a directory whose immediate children are Claude Code
config directories:

```toml
[anthropic]
accounts_dir = "~/.config/ai-usagebar/accounts"
```

Populate each account with the official CLI:

```bash
CLAUDE_CONFIG_DIR=~/.config/ai-usagebar/accounts/personal claude
CLAUDE_CONFIG_DIR=~/.config/ai-usagebar/accounts/work claude
```

This is Claude Code's standard
[`CLAUDE_CONFIG_DIR`](https://docs.claude.com/en/docs/claude-code/settings)
layout. Each subdirectory becomes an account named after the directory.

- Linux stores `.credentials.json` inside the account directory.
- macOS stores a config-dir-scoped Keychain item.
- ai-usagebar reads and refreshes each source independently.
- Explicit `[[anthropic.accounts]]` entries override discovered accounts with
  the same label.
- A missing or unreadable `accounts_dir` is ignored.

Any account manager that uses the same directory layout can share these logins
with ai-usagebar.

## Use existing credential files in Waybar

This lower-level setup is for credential files you already manage. Prefer
`account add` for new logins so you never copy an active refresh token.

```jsonc
"modules-right": ["custom/claude-personal", "custom/claude-work", ...],

"custom/claude-personal": {
    "exec": "ai-usagebar --vendor anthropic --icon '󰚩' --format 'p {session_pct}% · {session_reset}'",
    "return-type": "json",
    "interval": 300,
    "tooltip": true
},
"custom/claude-work": {
    "exec": "ai-usagebar --vendor anthropic --icon '󰚩' --format 'w {session_pct}% · {session_reset}' --creds-path ~/.config/ai-usagebar/accounts/work.credentials.json --cache-dir ~/.cache/ai-usagebar/anthropic-work",
    "return-type": "json",
    "interval": 300,
    "tooltip": true
}
```

Keep these rules in mind:

- `--creds-path` must point to an independently managed Claude OAuth file.
  Refreshes are written back to that file.
- Never run two clients against copies of the same refresh token. Token
  rotation will eventually strand one copy.
- Keep credential files at mode `600`.
- Give each module a separate `--cache-dir`.
- `--creds-path` is Claude-only. For API-key providers, use a wrapper that
  exports the account's key and give each module its own cache directory.

On macOS, prefer `accounts_dir`; scoped Keychain items avoid copied credential
files entirely.

## Switch the active account on macOS

Usage reporting and the active login are separate. macOS has two independent
Claude identities:

- Claude Desktop, signed in through its own `config.json`;
- the `claude` CLI, whose default login lives in the login Keychain.

Use the same label for both if they belong to the same account:

```bash
ai-usagebar account add work
ai-usagebar account add work --desktop
ai-usagebar account status
ai-usagebar account status --json
ai-usagebar account switch work --dry-run
ai-usagebar account switch work --desktop
ai-usagebar account switch work --cli
```

Without `--desktop` or `--cli`, `switch` handles both identities. If a label
exists on only one side, the missing side is skipped.

### Capture a Desktop account

The CLI supports isolated logins through `CLAUDE_CONFIG_DIR`. Claude Desktop
has only one login slot, so `account add <label> --desktop` must:

1. save the current Desktop login as a profile;
2. sign out and wait for the new login;
3. capture what Desktop writes;
4. seed the new profile with this machine's existing history.

Ctrl-C or a five-minute timeout restores the original login. CLI and Desktop
use different OAuth clients, so each identity must be captured separately.

### Switch Claude Desktop

Switchboard previews the history merge, quits Desktop, and recomputes the merge
after shutdown so newly saved changes are included. It then merges history,
swaps the credential and browser state, and reopens the app. Deletion decisions
are retained only while their observed account topology remains unchanged.

Every switch creates a rollback archive in `~/.claude-acc/backups/`:

- `--keep-backups N` controls retention (default: 10).
- `--backup-sessions` includes the full session tree.
- On Unix, the directory is mode `0700` and archives are mode `0600` because
  they contain credentials and browser state.

Before changing account state, Switchboard also makes best-effort private copies
of HTML/Markdown artifact sources referenced by native `frame-link` records in
known Desktop conversation transcripts under `~/.claude/projects/`. Copies live
in `~/.claude-acc/backups/artifact-copies/` (beside a custom profile store when
configured). There is no setting, button, viewer, or network request. Transcripts,
workspace paths, hosted links, and comment monitors are left unchanged.

Copies retain source metadata and distinct content revisions; repeat switches
deduplicate unchanged content. They are separate from rollback archive retention
and are never automatically deleted. Protection is bounded: 16 MiB per source,
256 MiB total copies, the last 8 MiB per transcript and 64 MiB per scan, at most
4096 entries per directory, with a two-second cooperative scan budget. Missing,
changing, unsupported, or unreadable sources and exhausted limits are skipped;
backup failure does not fail switching. This is content preservation, not a
guarantee of complete artifact recovery. Copies represent the local file at
switch time; externally referenced assets are not bundled. Claude-hosted artifacts
may still be inaccessible from another account. No cloud artifact is republished,
and a saved source does not restore its original card or comments.

The switch clears `bridge-state.json` because stale cloud-session ids can stop
`/remote-control` from disconnecting. Pass `--keep-bridge` only when testing
that behavior.

### Switch the CLI

The CLI has one default credential slot. A switch first saves the outgoing
credential under its account, then moves the target credential into the
default slot. ai-usagebar reads an active account from that default slot, so a
rotating refresh token is never live in two places.

If the current CLI login is not managed by ai-usagebar, the switch stops before
discarding it. `--force` overrides that safeguard and removes the unmanaged
login.

### Storage and history conflicts

CLI accounts use `[[anthropic.accounts]]` or `accounts_dir`. Desktop profiles
use claude-acc's format under `~/.claude-acc/profiles`; override that path with
`[anthropic] desktop_profiles_dir`. Existing claude-acc profiles work as-is.

A chat deleted inside Claude stays deleted. The app records each deletion as a
`deleted_<id>` marker beside the account's session indexes and consults those
markers before re-adopting a transcript. A switch honours them everywhere: the
chat's index leaves every account without a prompt, and the marker is copied
into every account/org folder so no account's import scan brings the
conversation back. A chat imported again after its deletion is newer than the
marker and is kept. Transcripts under `~/.claude/projects/` are never touched.

History merges can still expose deletions with no marker — an index that
vanished under an older app version or another tool. When that happens,
ai-usagebar asks whether to keep every copy, delete the item from all accounts,
or decide one item at a time. Deleting a chat removes only its index.

Non-interactive switches always keep conflicting items. The macOS menu bar
shows the same choices in a dialog. For scripts, `account status --json` lists
pending `deletion_conflicts`; pass the returned opaque key through
`--delete-conflict <key>`. Keys are scoped by item type, so a routine id cannot
authorize deletion of a chat with the same id.

Chat resume content reconciles by `lastActivityAt`; archive state reconciles
separately because Claude does not advance that timestamp when archiving or
unarchiving. Routine baselines retain each organization separately. Independent
field edits can merge, and enabled state reconciles separately from execution
bookkeeping. Conflicting changes to the same other routine field retain local
copies and remain reported as a conflict.

When old copies disagree without a trustworthy baseline, the merge keeps the
chat archived or routine paused. A later explicit unarchive or resume is tracked
and can propagate; these states do not permanently override new user edits.
New baseline fields are additive to the existing profile-store format.

Where a folder already carries the app's `archived-sessions.idx` load hint, the
switch regenerates it from the reconciled flags so the app never defers the
wrong chats.

### Sidebar groups and view mode

The Code sidebar's custom groups (shown as projects), their chat assignments,
and the grouping/sorting mode live in the renderer's `localStorage`, scoped to
one account and organisation. Restoring an account's saved browser state used
to restore whatever that account last showed — another grouping mode, or stale
empty groups with every merged chat under Ungrouped.

A switch now carries one reconciled sidebar. After the outgoing account's
browser state is saved, its scope is compared with the last observation of that
scope: groups added, renamed or deleted, chats moved between groups, reordered
members, and a changed view mode are folded into the canonical sidebar, which
is then written into the incoming account's scope before the app relaunches.
Groups persist until deleted. A scope that lost every group at once is not
treated as a deletion of all of them — that is also what the app's own server
merge produces — so deleting the last remaining group does not propagate.

Group definitions and the view mode are synced by the app to its server per
organisation, and a pull replaces local groups when the two differ. The switch
therefore also sets the app's own pending-edit marker for the store so the
first reconcile after launch pushes the merged sidebar instead of pulling the
stale one over it. Chat assignments are local-only and are never uploaded.
The store is rewritten through a staged copy that is read back before it
replaces the original; a failure leaves the target's browser state as saved
and is reported as a note. The `dframe-group-scopes` mirror in
`claude_desktop_config.json` is updated to match. Nothing is carried when the
incoming account has no saved browser state or no known organisation.

Claude-hosted artifacts and their comment monitors stay with the account and
organisation that published them; see the
[state investigation](claude-switch-state-investigation.md).

Account removal and chat filters (`only` / `reset`) are not implemented. Remove
a profile directory manually. Cowork sessions stay with the
account that created them because their transcript path contains the account
UUID.

Profile-format compatibility attribution is recorded in
[third-party notices](../THIRD_PARTY_NOTICES.md).
