# Claude artifact continuity investigation

Investigation date: 2026-09-16. Scope: the artifact/comment-monitor failure reported after two consecutive Claude Desktop account switches. This report contains source-code findings and sanitized local metadata only.

## What the merged change actually protects

[PR #6](https://github.com/timomak/switchboard/pull/6), merged as `260172d`, added private snapshots of local artifact source files. It does not transfer hosted artifacts between accounts, restore their cards, or restart their comment monitors. The change explicitly documents those limits in `docs/claude-accounts.md`.

The preservation pass runs after Desktop quits and before identity replacement. It finds known Desktop session IDs, scans their local JSONL transcripts for native `frame-link` records, and copies existing HTML/Markdown source files into `~/.claude-acc/backups/artifact-copies/`. It leaves source files, transcripts, hosted URLs, account permissions, and comment-monitor state unchanged. Switching itself contains no artifact-source deletion operation.

## Installed-build evidence

The locally installed helper in `~/Applications/Switchboard.app` reports app version `1.12.0`, build `31`. Its executable contains neither the `artifact-copies` nor `frame-link` marker strings from PR #6. Combined with the source implementation, this supports the finding that the running installation predates the merged artifact preservation change.

The helper's SHA-256 at inspection was `565e6ad94b005809b2df6ac8d976420ef8c69b6197b4d4fa8261da12398cddec`. The expected artifact backup directory did not exist. A merged PR therefore does not establish that the installed app performed preservation during these switches.

## Incident-specific local evidence

A read-only check used only the artifact identifier prefix supplied in the screenshot. It enumerated known Desktop session IDs from 1,171 local session indexes and searched 296 corresponding top-level transcript files. The identifier occurred in 13 lines of one transcript: three successful native `Artifact` tool results, four `artifact-comment-monitor` records, and six `artifact-autoreact-ledger` records. The latter records have an `accountUuid` field; its value was not exported.

The artifact ID in those records is different from its sharing-URL token. Correlating the tool results with their native `Artifact` calls identified three `frame-link` records for the same local source file. The source **still exists**, is a regular nonsymlink file, and is 58,393 bytes. Its contents were not displayed or copied into this repository.

Those frames use `https://claude.ai/artifact/<22-character alphanumeric token>`. PR #6 accepts only `https://claude.ai/code/artifact/<UUID>`, so it rejects all three observed frames even when installed. The frame field types, session identity, source extension, encoding, and size satisfy the other applicable checks; two of the frames are inside the last 8 MiB of the transcript. The route mismatch is therefore a separate reproducible defect from the missing installed update.

No transcript text, titles, source paths, account identifiers, hosted URL tokens, credentials, or source fingerprints were included in this report or synthetic tests. Files outside the explicitly scoped known-session transcript set were not searched.

## Why the visible failure can remain after PR #6

The screenshot establishes that a comment monitor could not restart in the resumed session. It does not establish loss of the underlying artifact content.

Claude's official documentation says the artifact gallery is read from the current claude.ai account; new artifacts are private; editing rights are granted separately; and automatic comment monitoring belongs to a running session and depends on service and feature availability. It also says artifact source files normally reside in temporary directories outside the project. Copying a conversation index and its transcript cannot give a different account access to the original hosted artifact. Account permissions are a plausible explanation here, but the specific server-side reason for the failed monitor was not observed. [Claude Code artifact documentation](https://code.claude.com/docs/en/artifacts)

## Fixes possible without changing Switchboard's interface

- Build and install the merged source-preservation change, keeping deployment verification distinct from merge status.
- Accept the observed native short-URL route alongside the existing UUID route. The implementation continues to require native `frame-link` records, the known session ID, the exact HTTPS host, the observed 22-character alphanumeric token shape, and the existing source-file checks. Extra URL path segments, queries, fragments, host lookalikes, and credentials remain rejected.
- Keep hosted artifact identity, permissions, and monitor state outside local history reconciliation. Do not silently republish artifacts under a different account: that can change ownership, URLs, versions, and comment continuity.

The existing snapshot implementation is deliberately bounded: 16 MiB per source, 256 MiB total, the last 8 MiB per transcript, 64 MiB per scan, 4,096 entries per directory, and a cooperative two-second scan budget. Missing, unreadable, changing, unsupported, or out-of-budget sources are skipped. Copies preserve local file contents at switch time; they cannot recover an already-deleted temporary source or guarantee the same hosted artifact and comments across unrelated accounts.

The short-URL correction is covered by synthetic regression fixtures that preserve source content and leave transcript/monitor state byte-identical. Rejection cases cover wrong origins, credential-bearing URLs, malformed tokens, and extra URL components. The correction changes no app interface, source paths, monitor state, credentials, or remote artifact.

Local source preservation and recovery can be improved entirely in the backend. The reported artifact's local source is available now. Seamless continuity of the original hosted artifact and its comment monitor additionally requires access supported by Claude, such as appropriate sharing/editing rights; local file copying cannot supply that permission.
