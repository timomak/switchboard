# Codex saved-task provider compatibility

New Azure OpenAI and OpenAI-compatible connections receive a persisted, random
`switchboard_<id>` provider identity at registration and a distinct
`SWITCHBOARD_KEY_<ID>` runtime credential variable. Neither is derived from the
label or secret. Renaming a label does not change routing. Existing records are
not silently migrated from `custom` or `azure`.

Selecting a default and retaining a saved task's dependencies are separate
operations. On subscription return or connection A → B, Switchboard retains
referenced managed definitions and their exact credential assignments in the
same home's `config.toml` and private `.env`. It restores the subscription's
default/model settings and preserves unrelated settings. Conversation bodies,
provider metadata, pins, projects, workflows and authentication files are not
rewritten. If no tasks reference the outgoing connection, the original config
and environment are restored without adding retention material.

References are the union of read-only `state_<number>.sqlite` thread provider
IDs and first-line metadata in `sessions/` and `archived_sessions/`. Unindexed
and archived tasks count. Unreadable stores, unknown queried schemas, symlinks
and malformed metadata stop switching with a value-free error. References are
checked again after shutdown; newly created managed dependencies are added to
the exact journaled write. Recovery also preserves managed dependencies and
refuses outside edits or ambiguous credential rebinding.

## Credential lifetime and changes

Selecting a connection copies its selected source key into the private runtime
`.env`. Saved tasks keep that copy available after returning to subscription,
so resuming them can still contact and charge the original provider. The source
file is read on selection, never edited. Removing or rotating the source file
alone does **not** revoke a retained runtime key. Private recovery snapshots also
contain credential copies; private permissions are not encryption.

Retention has no automatic expiry or garbage collection in this version.
Retained credentials remain until explicitly retired locally or revoked at the
provider. Revoke credentials at the provider to end access, then, with Codex and
its CLI sessions stopped, remove the corresponding managed definition and
credential line together from the home and retire its saved connection record
and private recovery snapshots. Keep a private backup if manual recovery is
needed. Removing dependencies makes those historical tasks unavailable; no
fallback or automatic migration is supplied. Do not publish these files.

Changing a referenced identity's endpoint or key is refused, including when the
new key might be a rotation within the same account: Switchboard cannot prove
that account relationship from a key string. Restore the original source fields
or register a new named connection for future tasks. Historical tasks retain
the old definition/key; a revoked key fails authentication. Rebinding historical
tasks to a rotated key or another endpoint requires deliberate manual migration
and is not automated. A missing runtime credential stops a managed transition;
it never borrows another connection's credential.

## Adapter scope

| Adapter | Saved-task switching behavior |
| --- | --- |
| New OpenAI-compatible Responses connection | Stable identity, per-identity key, retained definitions; subscription return and A → B supported. |
| New Azure OpenAI connection | Same retention; resource endpoint normalized to `/openai/v1`; the configured model is an Azure deployment ID. |
| Existing `custom` / `azure` records | Legacy guard remains. A reference alone cannot establish the original endpoint/account. Returning to subscription may be refused; register a new connection for future tasks. |
| Codex Amazon Bedrock | Native `amazon-bedrock` adapter uses shared AWS token/region variables. No claim of simultaneous independent AWS identities. Referenced tasks can block subscription return or credential changes; safe refusal is not successful switching. |
| Claude Bedrock | Separate native third-party workspace and credential helper; not a Codex provider adapter. Subscription chats stay in their subscription workspace. One saved connection is supported. |

No provider model access, OAuth login, AWS account access or arbitrary model
compatibility is inferred from these local checks. Existing workflow model
overrides may need adjustment by their owner. External homes, offline/removed
stores, independent editors and future runtime layouts are outside this guard.

## Evidence

Hermetic Rust fixtures cover both managed adapters, subscription → A → saved
task → subscription, A → B with identical source variable names, archived and
DB-only tasks, creation during shutdown, endpoint/key rebinding, missing source
and runtime credentials, partial-write recovery, outside edits, legacy guards,
and Bedrock's shared environment. They assert unchanged conversation and DB
bytes. Claude connection recovery and capture admission have separate tests.

`python3 scripts/test-codex-provider-runtime.py` is an opt-in probe of the official
runtime installed on PATH. On Codex CLI **0.151.0**, its app-server implicitly
resumed A after the default returned to built-in OpenAI or changed to B, keeping
A's provider, model, loopback URL and distinct key. B used its own Azure-shaped
`/openai/v1` URL and key. Simulated 401 and missing-key cases failed without
falling back to the default. The probe uses disposable homes, synthetic keys and
a loopback Responses server; it does not exercise paid inference, Azure service
access, subscription OAuth or the desktop UI. Existing user-tested switching is
not repeated or replaced by this evidence.

See the official [provider configuration](https://learn.chatgpt.com/docs/config-file/config-advanced)
and [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).
