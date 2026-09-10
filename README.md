# Switchboard

Claude and Codex accounts, within reach. Switchboard is a native macOS menu bar app with separate Desktop and CLI account selectors and account-specific usage.

![Switchboard with example accounts](screenshots/switchboard-macos.png)

- Manage Claude and Codex accounts from the menu bar.
- Select Desktop and CLI identities separately, or share the Codex Desktop login with CLI.
- See Claude five-hour and weekly usage and Codex weekly usage.
- Manage saved accounts and API connections from Settings.
- [Continue a conversation in another app](docs/continue-in-another-app.md) by cloning supported local chat histories into independent native sessions.

Switchboard is independent of Anthropic and OpenAI.

## Build

Install Apple's Command Line Tools and Rust 1.88 or newer:

```sh
git clone https://github.com/timomak/switchboard.git
cd switchboard
./macos/bundle.sh
```

The app and ZIP are written to `dist/`. Builds enable account switching and shortcuts by default (`--switching-enabled`). Use `./macos/bundle.sh --preview` for a guarded UI preview; its ZIP has a `-preview` suffix. Building does not install or launch the app. Backend commands are described in the guides below.

For Developer ID signing and notarization, see [macOS distribution](docs/macos-distribution.md). Signing credentials belong in your local Keychain and are not included in this repository.

## Account behavior

Claude Desktop and CLI logins are independent. Codex CLI can share the Desktop home or use a separate authenticated home. Use the backend CLI launcher when commands should follow the selected CLI mode; an ordinary `codex` invocation follows its own environment.

Saved tasks can depend on a specific provider. New Compatible and Azure connections retain their own provider definitions and credentials when returning to subscription or selecting another connection. Legacy connections and Codex Bedrock still refuse changes that would remove or rebind a referenced provider, which can block subscription return. See [provider compatibility](docs/codex-task-provider-compatibility.md) for details.

- [Desktop and CLI guide](docs/desktop-cli-switchboard.md)
- [Claude accounts](docs/claude-accounts.md)
- [Codex profiles](docs/codex-desktop-profiles.md)
- [API connections](docs/codex-cloud-providers.md)
- [Recovery](docs/switchboard-recovery.md)

## Privacy

Credentials stay in the existing local profile stores and Keychain locations. Some profile files, runtime configuration and recovery snapshots contain credentials; private file permissions are not encryption. Do not upload account stores, dotenv files, backups or raw diagnostics to issues. The README screenshot uses synthetic accounts.

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked --lib codex_account::
cargo test --locked --lib codex_provider::
cargo test --locked --lib claude_connection::
cargo test --locked --lib claude_desktop::capture::
./macos/run-tests.sh
```

With Gitleaks 8.28.0 installed, run `./scripts/scan-secrets.sh` and `./scripts/scan-secrets.sh --history`. CI checks source and history with redacted output.

## License

Original contributions use the [Switchboard Permissive License 1.0](LICENSE). Incorporated components retain their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md). Provider marks belong to their respective owners.

The `ai-usagebar` executable names and existing storage paths are retained for compatibility. Inherited Linux frontends remain in the source tree; this repository does not automatically publish their packages.
