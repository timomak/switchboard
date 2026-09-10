# Install this fork on macOS

Build with Rust and Xcode Command Line Tools installed:

```bash
git clone https://github.com/timomak/ai-usagebar.git
cd ai-usagebar
./macos/bundle.sh
open "dist/AI Usage Bar.app"
```

The build produces an app bundle and an architecture-specific ZIP in `dist/`.
It includes the matching `ai-usagebar` and `ai-usagebar-tui` binaries. The bundle
is locally ad-hoc signed, not notarized for public distribution. Building does
not install the app, change profiles, or restart any running application.

To keep it, quit any older AI Usage Bar instance, copy the new bundle to
`~/Applications`, and open it there. This fork uses its own preference domain
(`io.github.timomak.ai-usagebar`); existing Claude profile files are reused.
Choose **Preferences → System → Start at login** after placing the bundle in
its permanent location. The bar starts with one compact icon. Click it for the
Claude and Codex Desktop/CLI selectors. Display settings can show provider names
beside the icon and toggle weekly usage. See the [switchboard guide](../docs/desktop-cli-switchboard.md).

**Preferences → System → Binary path** is an explicit override. Leave it empty
to use the backend bundled with this fork; a path to an older upstream binary
will not provide Codex switching. `cargo install ai-usagebar` fetches upstream,
so it is not the installation command for this fork.

The two account menus are **Claude Desktop** and **Codex Desktop**. **Usage
display** controls only which usage numbers are shown. See the
[profile guide](../docs/codex-desktop-profiles.md) for initial setup and recovery.

To update, pull this fork, rebuild, quit the menu app, and replace its bundle.
No account export or migration is required. Removing the menu app does not
remove Claude or Codex, their local history, or saved profiles.
