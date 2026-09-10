# Install Switchboard on macOS

Download the latest signed and notarized macOS archive from
[Switchboard releases](https://github.com/timomak/switchboard/releases/latest).
Extract it, quit Switchboard if it is running, and move `Switchboard.app` into
`~/Applications`, replacing the previous copy. Open the app from that location.

## Build locally

With Rust and Xcode Command Line Tools installed:

```bash
git clone https://github.com/timomak/switchboard.git
cd switchboard
./macos/bundle.sh
open "dist/Switchboard.app"
```

The build produces an app bundle and an architecture-specific ZIP in `dist/`.
Local builds are ad-hoc signed; official releases are signed and notarized.
Building does not install or launch the app. To install a local build, quit
Switchboard and replace its bundle in `~/Applications`.

## Setup and updates

Choose **Preferences → System → Start at login** after placing the app in its
permanent location. Leave **Binary path** empty to use the bundled backend.
The application identifier is `io.github.timomak.switchboard`.

Desktop and CLI selectors manage accounts independently of usage display.
See the [Desktop and CLI guide](../docs/desktop-cli-switchboard.md) and
[conversation guide](../docs/continue-in-another-app.md).

Updates preserve existing profile stores. Removing the menu bar app does not
remove connected applications, their histories, or saved profiles.
