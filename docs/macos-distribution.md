# Local signed distribution

These commands build separate candidates under `dist/distribution/`. They never install or launch an app, change live account/provider stores, or publish a GitHub release. Switching is enabled by default; pass `--preview` to build a guarded candidate. They do not need an App Store Connect app record, App ID change or provisioning-profile change.

## Sign

Use an existing valid Developer ID Application identity from `security find-identity -v -p codesigning`. Do not revoke/recreate unrelated certificates or export their private keys.

```sh
SWITCHBOARD_SIGNING_IDENTITY='Developer ID Application: Your Name (TEAMID)' ./macos/package-distribution.sh --switching-enabled
```

Commit source changes first. The script records the source commit in the bundle and package directory, builds the requested mode, signs the two bundled executables and app explicitly from the inside out with secure timestamps and hardened runtime, and verifies their signatures. It retains existing executable/helper paths. The sole entitlement is Apple Events automation for the existing desktop-app control behavior. This is not a sandbox entitlement or permission to access every app. macOS may ask for Automation permission when you first restart a desktop app.

For a synced source checkout, set `SWITCHBOARD_DISTRIBUTION_ROOT` to a local,
non-synced staging directory. This avoids cloud-file metadata being added to the
signed candidate while notarization is processing.

Each invocation produces a separate `package.*` directory containing the signed app, `submission.zip`, and exact app/archive hash manifests. A signed candidate is not yet notarized or ready for a general download. Never publish this directory wholesale.

## Configure authentication once

In your Apple Account's Sign-In and Security settings, generate a dedicated app-specific password labelled Switchboard notarization. Do this yourself and never paste it into chat, source, logs or command arguments. Do not revoke other app-specific passwords or change the account password.

Run in Terminal:

```sh
./macos/setup-notarization.sh
```

Supply the Apple Account email and Developer Team ID, then enter the app-specific password at Apple's hidden prompt. The tool validates with Apple and saves the credential as the local Keychain profile `Switchboard-notarization`; the scripts never receive or write its raw value. Apple account/security steps may require separate sign-in or 2FA even when App Store Connect is already open.

## Submit and finish

```sh
SWITCHBOARD_NOTARY_PROFILE=Switchboard-notarization ./macos/notarize.sh /absolute/path/to/dist/distribution/package.EXAMPLE
```

This submits the signed candidate to Apple, preserving the submission ID for retries, and waits up to 60 seconds. Re-run with the same directory if processing continues; it resumes the saved submission. An uncertain failed upload blocks automatic re-upload until notarization history is reconciled. Do not delete its marker merely to force a retry. Any change to the signed app/archive requires a fresh candidate.

After Apple reports Accepted, the script staples/validates the ticket, verifies signatures and Gatekeeper assessment, then creates a versioned ZIP and `SHA256SUMS`. Publish only that final archive/checksum after the separate release approval. The submission archive and service metadata are not release assets.

No successful notarization or Gatekeeper acceptance is claimed until those commands pass. Installed-app replacement and a real quarantined-download launch remain separate local actions. A previous preview signature does not cover rebuilt switching-enabled bytes.

## Install, recover, and uninstall

The current candidate is built and verified on Apple Silicon with macOS 26.6.1.
Older macOS versions are not verified; inspect the packaged executable deployment
target before claiming support. Quit the installed
Switchboard before replacing it, then extract the final ZIP and move
`Switchboard.app` to Applications. Grant Automation access for the desktop apps
you choose to restart when macOS asks. Do not run two switchers simultaneously.
Use [recovery commands](switchboard-recovery.md) if a change was interrupted;
never delete a pending journal to bypass a recovery error.

To uninstall, disable Launch at Login in Settings, quit Switchboard, and remove
the app. Existing account stores and runtime provider settings remain. Return
to subscription first if appropriate; retained provider tasks/credentials follow
the [retirement policy](codex-task-provider-compatibility.md). Keep private backups
for rollback. There is no automatic updater or universal/Intel candidate in this
release.
