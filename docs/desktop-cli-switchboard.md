# Desktop and CLI switchboard

Click the split Claude/OpenAI menu-bar icon to open Switchboard. Settings is at the lower left, Quit at the lower right, and Manage accounts is inside Settings. The current preview blocks account changes and global shortcuts. The native popover has one Claude
card and one Codex card, each with Desktop and CLI rows. Usage belongs to the
account named above it; an independent CLI account gets its own usage line.
Missing usage is displayed as unavailable, never zero. Refresh reads identities
first and drops responses superseded by an account operation. The popover is sized
against the current display before opening; smaller displays scroll vertically.
A display change dismisses it so the next opening uses the new menu-bar anchor.

## Claude

Desktop switching retains the existing history/routine reconciliation and
restart confirmation. The CLI row controls the login used by ordinary `claude`.
If the CLI is already signed in, **Set up → Save CLI login** registers its
identity without copying or replacing credentials, transcripts, or settings.
Additional CLI accounts sign in through official Claude in Terminal. Desktop
and CLI require separate authentication; a Desktop snapshot is not a CLI login.

```sh
ai-usagebar account save personal
ai-usagebar account add work
ai-usagebar account switch work --cli --yes
ai-usagebar cli launch claude
```

Finish ordinary CLI work before changing its default login. CLI sessions opened
through the launcher hold a lease that blocks credential switching until they
exit. Sessions launched outside this utility are not tracked by that lease.

## Codex

**Shared with Desktop** uses the existing Codex home, preserving the existing
local workspace. Desktop account changes also change this shared stored login.

**Separate CLI account** signs in independently using the official OAuth flow.
It uses `~/.claude-acc/codex-cli/home` for one dedicated CLI workspace, and keeps
saved credentials/recovery records under `~/.claude-acc/codex-cli`. No Desktop
auth, chats, settings, skills, databases, or routines are copied into it. CLI
history starts separately and remains in that workspace across CLI switches.
The first successful setup selects separate mode; additional accounts are saved
for selection in the CLI menu. A CLI switch selects separate mode as well.

```sh
ai-usagebar codex-account add work --cli
ai-usagebar codex-account status --cli --json
ai-usagebar codex-account switch work --cli --yes
ai-usagebar cli mode separate
ai-usagebar cli launch codex
ai-usagebar cli mode shared
```

The **Open CLI** button launches official Codex in Terminal with the selected
home. Ordinary `codex` commands continue using their usual environment. For a
terminal command that follows the menu selection, use `ai-usagebar cli launch
codex`; extra CLI arguments can follow `--`. The launcher does not alter shell
profiles or replace either official binary. It removes inherited token/home
overrides that would silently defeat the selected login.

CLI switches have their own profile store, operation lock, session lease, and
recovery journal. They stop only the dedicated CLI daemon, never Desktop. Close
CLI sessions launched by this utility first; their leases block switching.
Changing shared/separate mode affects future launches and leaves running
sessions on their original workspace. Direct launches outside this utility and
unattended/background work need to be finished separately before switching.

An interrupted switch exposes **Recover interrupted switch**. From Terminal:

```sh
ai-usagebar codex-account recover --cli --yes
```

The existing Desktop command forms remain supported without `--cli`. Both modes
require file-backed subscription OAuth credentials; no API-key or Keychain
account conversion is performed. Status contains account metadata, never tokens.

## Build and checks

```sh
./macos/bundle.sh
./macos/run-tests.sh
cargo test
cargo clippy --all-targets -- -D warnings
```

`macos/switchboard-preview.swift` renders the real native view with fixture
accounts. Compile it together with the two app Swift files and
`-D SWIFT_TEST_HARNESS`, then supply an output PNG path and optionally `--dark`
or `--separate`; `--short-screen` exercises a 450-point-high display. The harness never polls accounts or reads credentials.

Building does not install or launch the app. The bundle uses `io.github.timomak.switchboard`. Follow the [installation guide](../macos/INSTALL.md) to replace an installed copy. Existing profile paths and credential-helper references remain compatible.
