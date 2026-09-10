# Claude and Codex desktop profiles

This fork keeps ai-usagebar's usage monitor and adds a separate Codex Desktop
profile selector beside the existing Claude Desktop selector. The macOS UI is
English. Both apps remain the official desktop applications.

## Switchboard

- Click the compact menu-bar icon to open the native **Accounts** popover.
- **Claude → Desktop** selects an existing Claude profile or captures another one using ai-usagebar's existing workflow.
- **Codex → Desktop** saves the current login, signs into another account, or switches to a saved profile.

Each card also has a **CLI** row. Claude CLI is independent; Codex CLI defaults
to **Shared with Desktop** and can use a dedicated workspace. See the
[Desktop and CLI switchboard guide](desktop-cli-switchboard.md).

## First Codex setup

1. Sign into the official Codex desktop app normally.
2. Choose **Codex Desktop → Save current account…** and name it, for example `personal`.
3. Choose **Codex Desktop → Add account…**, name it `work`, and complete sign-in in your browser. This uses an isolated temporary Codex home and leaves the running app logged into `personal`.
4. Finish active Codex tasks, then choose **Codex Desktop → work → Switch and restart**.
5. Select `personal` the same way to switch back.

Profiles are stored under `~/.claude-acc/codex-profiles/`. Claude's existing
profile store is unchanged. Only the Codex login file is replaced in the active
Codex home (`~/.codex` by default). The switcher never copies, removes or merges
Codex session transcripts, project configuration, databases, or automation
definitions. Local availability in the app still depends on the official
client and the selected account's permissions; this does not transfer cloud
account ownership or subscription entitlements.

Requires macOS, the official Codex desktop app, a Codex executable supporting
`app-server --stdio`, and file-backed ChatGPT authentication. API-key accounts
and Keychain-backed Codex authentication are not supported. An explicitly
configured `cli_auth_credentials_store` other than `file` stops the operation;
the switcher does not edit your Codex settings. Use the same `CODEX_HOME` as your
desktop app if you customize it; a Finder-launched menu app does not inherit
Terminal-only environment settings. `CODEX_CLI_PATH` can select a specific
Codex executable. Otherwise the bundled Codex app executable, PATH, and common
Homebrew locations are checked.

## Failure handling

Switches are serialized with a local file lock. A shared `auth.ai-usagebar.lock`
also coordinates switching with ai-usagebar’s token refreshes, and the usage
cache is tied to an opaque account identity so old quota cannot follow a switch. Before changing authentication,
the backend validates the target, requires the outgoing login to be registered,
and asks Codex to quit gracefully. It never force-kills the desktop app. After
quitting, it saves the latest outgoing tokens, writes a recovery record, and
atomically replaces `auth.json`. Codex's official `account/read` protocol and
the resulting auth-file identity must agree with the selected profile. This
checks which login the client loads; it does not prove that a model request
will succeed or that a subscription has capacity.

Verification failure restores the previous login and reopens the app. An
interrupted switch leaves **Recover interrupted switch…** in the menu. Recovery
refuses to overwrite a third, unrelated account. If only reopening fails, the
new profile is already active and Codex can be opened manually. Finish tasks
and avoid concurrent logins or CLI token refreshes during a switch.

The profile store uses private directories (`0700`) and credential files
(`0600`). Status output contains labels and identity metadata, never tokens.
Deleting an inactive saved profile removes its reusable login snapshot, not
local Codex chats. Keep this store out of Git and public backups.

## CLI

Use the backend bundled with Switchboard:

```bash
./target/release/ai-usagebar codex-account status --json
./target/release/ai-usagebar codex-account save personal
./target/release/ai-usagebar codex-account add work
./target/release/ai-usagebar codex-account switch work --dry-run
./target/release/ai-usagebar codex-account switch work --yes
./target/release/ai-usagebar codex-account recover --yes
./target/release/ai-usagebar codex-account remove work # only when inactive
```

## Architecture

Rust owns credentials, isolated login, process orchestration, and recoverable
account transactions. Swift consumes non-secret status JSON and dispatches
commands; it never opens a credential file. Switching validates the profile,
saves outgoing tokens, activates the selected account, verifies it, and rolls
back on failure. Required attributions are in
[third-party notices](../THIRD_PARTY_NOTICES.md).

## Validation

`make test`, `cargo clippy --all-targets --locked -- -D warnings`, and
`./macos/run-tests.sh` cover the existing app and this port. Codex tests use
temporary homes and fake app lifecycle/RPC processes. They cover preserving
local data, rotated outgoing tokens, rejected quits, mismatched identities,
rollback, interrupted recovery, profile permissions, secret-free status, and
independent menu state. Live account switching must be checked separately from
a terminal or after the current Codex task finishes, because it closes Codex.
