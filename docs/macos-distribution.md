# Local signed distribution

These commands build separate candidates under `dist/distribution/`. They never install or launch an app, change live account/provider stores, publish a GitHub release or disable the preview guard. They do not need an App Store Connect app record, App ID change or provisioning-profile change.

## Sign

Use an existing valid Developer ID Application identity from `security find-identity -v -p codesigning`. Do not revoke/recreate unrelated certificates or export their private keys.

```sh
SWITCHBOARD_SIGNING_IDENTITY='Developer ID Application: Your Name (TEAMID)' ./macos/package-distribution.sh
```

The script builds the preview, signs the two bundled executables and app explicitly from the inside out with secure timestamps and hardened runtime, and verifies their signatures. It retains existing executable/helper paths. The sole entitlement is Apple Events automation for the existing desktop-app control behavior. This is not a sandbox entitlement or permission to access every app. Live TCC/permission behavior still needs approved testing.

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

No successful notarization or Gatekeeper acceptance is claimed until those commands pass. A real quarantined-download launch and permitted account/provider tests remain separate acceptance checks; keep the preview safeguard in place until cutover is authorized.
