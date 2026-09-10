# Bounded recovery behavior

Recovery commands change live state. Finish active work before running recovery.

Claude Bedrock selection now acquires the same configured account-switch lock used by ordinary Claude Desktop switching, as well as its existing connection lock. It retains `desktop-before.json` in the existing location. Immediately before quit it writes `desktop-pending.json` with the before/after selection bytes using the existing private atomic-write helper. Quit failure leaves that record; write failure still attempts relaunch; write or relaunch failure keeps recovery required. Retrying selection cannot overwrite that journal or the original backup.

`ai-usagebar cli claude-recover` restores the selection before the interrupted operation, including its absence. It checks for outside edits both before and after quitting, relaunches after attempted restoration, and retains the journal if recovery/relaunch fails. A successful retry removes the pending record. Status inspection never triggers this operation.

The selection journal is deliberately narrow: it covers `_meta.json`, not every Claude file, helper or generated provider configuration. Inactive helper/config preparation can remain after an early failure. Existing subscription backup and helper locations remain compatible. Provider credentials, workspace semantics and saved connection schema are unchanged. No source-credential or selection value is included in error messages.

Ordinary Desktop switches refuse a pending connection change. Other tools and direct official-app actions do not honor this journal; the account-capture path shares the lock but does not interpret the connection journal. Finish connection recovery before capturing accounts or using other switching tools. Outside selection edits cause recovery to stop for manual review, never automatic merging. The switcher does not guarantee recovery from disk loss, malicious file edits, or all official-app behavior changes.

Codex account recovery remains `ai-usagebar codex-account recover --yes` (add `--cli` for the separate CLI home). Codex provider recovery remains `ai-usagebar codex-provider recover --yes`. These existing guards remain intact. An owned-field conflict is not permission to discard intentional configuration changes: preserve them and reconcile the named field locally before retrying.

Codex provider recovery also honors saved-task provider dependencies. If rollback would remove or rebind a provider referenced by saved task metadata, recovery stops and retains its pending record. Do not clear that journal or rewrite task providers indiscriminately. Follow the [provider compatibility policy](codex-task-provider-compatibility.md) for deliberate retention/migration design.
