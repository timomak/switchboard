# Codex saved-task provider compatibility

## Immediate policy implemented in this cleanup

A saved task's provider ID is a persistent routing dependency. Preserving conversation files while removing or replacing that dependency is not compatible switching.

Before applying a connection transition, inspect provider bindings from versioned `state_<number>.sqlite` databases using read-only connections and from the first metadata line of rollouts in `sessions/` and `archived_sessions/`. Union both sources; agreement is not assumed. Do not parse conversation bodies, follow symlinks, rewrite rows or rewrite rollouts. Unreadable stores, unknown queried schemas or malformed metadata block the operation with a value-free error.

If a referenced provider definition would be removed, introduced under a previously unresolved referenced ID, or changed, refuse the transition. If a referenced configured provider remains defined but the runtime environment changes, also refuse: credential rebinding can change the destination account even when the URL is unchanged. This environment comparison is intentionally conservative and may reject unrelated environment edits.

Recheck after quitting, before writing configuration or the pending journal, because a task can be created between planning and shutdown. Apply the same check during recovery so rollback cannot bypass the policy. Keep the existing configuration/credential environment and task records untouched on refusal. No automatic migration, credential retention, or fallback to another provider/model is permitted.

This is a conservative admission guard, not seamless cross-provider continuation. Returning to subscription after creating a provider-bound task can now be blocked. The fixtures model `subscription → compatible → create task → request subscription → safe refusal → resolve/reopen against the preserved provider`; they do not claim successful subscription return or a live Codex reopen. Switching between two compatible endpoints under the reused `custom` ID is likewise refused when referenced. Existing round-trip tests without provider-bound tasks still pass.

## Deliberate design for eventual successful return

1. Give each saved routing identity a persisted, opaque provider ID at registration. Do not use the shared `custom`/`azure` IDs for new identities or derive IDs from credentials. Renaming a display label must not change the provider ID. A changed endpoint, tenant/account scope, protocol or incompatible credential mapping creates a new identity; it must never silently repoint an existing ID.
2. Selecting a default provider and retaining definitions for historical tasks are separate operations. A subscription return may change the default/model settings only after every saved provider dependency remains resolvable. A referenced provider definition cannot be retired or reused without validated migration. An unreferenced definition can be retired only after accounting for archived/unindexed rollouts and concurrent task creation.
3. Define credential lifetime explicitly before retaining any runtime material. Compatible/Azure providers would need distinct supported credential bindings per provider identity, rotation semantics for the same account, and actionable errors when credentials are unavailable. Returning to subscription must not silently grant indefinite storage or access to old credentials. Bedrock's existing shared AWS environment cannot be assumed to support simultaneous identities. Verify the official runtime's behavior before enabling such retention. Until then, keep the admission guard.
4. Treat existing `custom`, `azure` and native adapter references as legacy dependencies. A name alone cannot prove endpoint/account/model provenance. Use authorized local recovery evidence to establish a mapping; never infer it from today's connection or globally rewrite those IDs.
5. Explicit migration must be per-task (or a specifically reviewed set), with proven source/target protocol and model compatibility, backups of the DB row and rollout, coordinated quiescence, matching metadata changes, unchanged conversation-body bytes, and rollback. Validate actual reopening only in an authorized live session. A successful one-off migration is not a universal model/provider compatibility claim.

Stable IDs, retained definitions, credential lifetime/retirement, and a migration UI are design work recorded here, not implemented or enabled by this bounded cleanup. They need separate review before a public compatibility claim or cutover. The current implementation neither changes the saved connection schema nor extends credential lifetime.

## Fixture coverage and limits

Tests cover DB+rollout references during subscription return; two endpoints using the same source-field names; DB-only and archived-rollout-only references created after preparation; invalid DB schema/data; credential-environment rebinding; and recovery refusal. They verify unchanged task/configuration bytes and local provider resolution. All paths are temporary; no provider requests, live SQLite stores, login flows or installed apps are used.

The guard recognizes the current file naming/layout and first-line metadata contract. It is not a general importer or a guarantee for external homes, removed/offline stores, future layouts, direct configuration editors or independently running tools. Unknown inspected schema/layout is a reason to stop and review, not to guess a provider. Official runtime auth/model access and full live reopen remain unverified.
