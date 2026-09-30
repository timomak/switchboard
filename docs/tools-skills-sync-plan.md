# Tools and skills sync — implementation plan

Historical implementation plan. Automatic chat sync was retired in build 35;
see [manual Mac transfers](mac-transfers.md) for its replacement. The tools and
skills library remains a separate feature.

Status: implemented for build 33. See [setup and current limitations](tools-skills-sync.md).
This document records the design; account plugin import and physical two-Mac
verification remain distinct from automated fixture checks.

## Outcome

Maintain one personal library of skills and MCP setups through iCloud on two
MacBooks. Share the library across the user's accounts, with independent targets
for Codex, Claude Code, and Claude Cowork. Each app keeps its native settings,
authentication, and approval behavior.

The first release provides automatic local installation for Codex and Claude
Code, plus a versioned plugin and guided installation for Cowork. Automatic
Cowork account updates require a verified supported mechanism before inclusion.

## Product decisions

| Area | Decision |
| --- | --- |
| Cloud location | `iCloud Drive/Switchboard/Tools & Skills`, separate from chat packages. |
| Content | Complete selected skill folders and normalized MCP definitions. |
| Scope | Personal custom skills and selected personal MCP setups; explicit destination choices. |
| Accounts | Shared definitions; authentication bindings stay separate by app/account/server. |
| Local state | Installation receipts, path overrides, recovery journals, and credential references outside iCloud. Secret values remain in their existing local stores. |
| Updates | Import compatible edits from either Mac; use revision ancestry and hashes. |
| Conflicts | Keep both candidates and retain the last working installation until the user chooses. |
| Disable/removal | Version the change so another Mac cannot resurrect it. Remove only unchanged Switchboard-managed entries; preserve local modifications as conflicts. |
| Overall sync off | Pause transfer and retain installed items and saved revisions. |

## Milestone 1 — inventory and portable library

Deliver a read-only inventory and a small, versioned library format.

- Discover supported personal skill roots and MCP configuration locations using
  the installed clients and Switchboard's configured Desktop/CLI homes. Respect
  custom homes and deduplicate a store shared by Desktop and CLI.
- Classify items as custom, plugin-managed, account-managed, or unsupported.
  Import only selected custom items. Leave managed caches under their original
  updater, including Claude's account-skill download cache.
- Give each item a stable ID independent of its display name, kind, target set,
  origin, dependency declarations, and immutable content revisions. Adopt
  identical copies; surface same-name different-content collisions.
- Preserve complete skill folders, supporting files, executable bits, and
  source attribution. Check references, app-specific frontmatter/tool names,
  dependencies, bounded sizes, and unsafe paths before publishing.
- Represent MCP transport, endpoint or launch command, arguments, non-secret
  options, and required local bindings explicitly. Do not export whole native
  config files. Credential-bearing or ambiguous values remain local and produce
  a setup requirement; unsupported opaque configurations are not guessed at.
- Record requirements for local executables, paths, environment variables and
  authentication without installing dependencies or executing MCP commands
  during inventory.

Done when synthetic source inventories produce deterministic portable records,
identify unsupported items clearly, and make no native or cloud changes until
the feature is enabled and items are selected.

## Milestone 2 — automatic Codex and Claude Code sync

Deliver bidirectional transfer and installation through native adapters.

- Create `src/library_sync/` with separate model, storage, skill, MCP, and
  destination-adapter modules. Add a `library-sync` CLI for status, inventory,
  enable/disable, one sync pass, target selection, and conflict resolution.
- Reuse or narrowly extract chat sync's content hashing, immutable publication,
  bounded reads, atomic writes, and recovery helpers. Keep library schemas and
  receipts separate. Do not reuse `NativeSession`, native chat IDs, or automatic
  chat-fork conflict behavior.
- Install skills into supported native discovery locations. Map local paths
  through per-Mac bindings and maintain receipts linking each installed copy to
  its source revision. Avoid duplicate discovery through legacy roots/plugins.
- Translate the supported MCP subset into Codex and Claude Code configuration.
  Update only owned server entries, retaining unrelated settings and comments
  where the native format permits. Preserve Codex model/provider selection and
  Claude account settings.
- Coordinate with existing account/provider operation locks. For v1, apply
  native changes while the relevant clients are closed; revalidate destination
  files immediately before replacement, journal changes, and recover retries.
- Treat missing dependencies or sign-ins as per-destination setup states. Never
  label a copied definition as an authenticated, usable connection without
  evidence. Leave host trust and approval policies intact.
- Propagate explicit disable/removal revisions without deleting unrelated or
  locally modified content. Mere disappearance from a scan is not a cloud-wide
  deletion instruction.

Done when two synthetic Macs converge across supported account homes, retries
are idempotent, and local edits, credentials and unrelated settings survive.

## Milestone 3 — Cowork installation

Deliver an honest Cowork workflow using its account-based plugin support.

- Generate a deterministic, versioned Claude plugin ZIP from items compatible
  with Cowork, with the required manifest, skills and supported MCP definitions.
  Keep secrets and Mac-specific bindings out of the distributable package.
- Provide **Install/update in Cowork**: open the native installation flow and
  reveal the generated ZIP. Include a short setup step for each Claude account.
- Track exported version separately from observed or user-confirmed installation
  version. If installation cannot be verified, show **Install required** or
  **Update available**, not **Ready**.
- Verify one representative custom skill and one packaged MCP in Cowork, including
  scripts/assets and authentication behavior. A packaged local server must work
  in Cowork's execution environment, not merely on the host Mac.
- Treat service-owned connectors as app-specific when their authorization or
  endpoints are not portable. Give setup guidance instead of copying tokens.

Done when the generated package imports through Cowork's supported UI and its
components work. Silent account-level updates remain outside v1 unless an
official mechanism is verified during this milestone.

## Milestone 4 — UI, verification and release

Extend the existing sync area with a **Tools & skills** view:

- **Sync skills** and **Sync MCP setups** toggles.
- A compact item list with Codex / Claude Code / Cowork destination choices.
- Status per destination: **Ready**, **Waiting for app**, **Needs setup**,
  **Sign-in needed**, **Conflict**, **Install required**, or **Update available**.
- **Sync now**, conflict choice, and **Install/update in Cowork** actions.
- Reuse the existing minute cadence and account-operation exclusion. Opening the
  view stays read-only; enabling sync is explicit. Show initial discovery counts
  and selected items before adoption.

Required verification uses synthetic fixtures and focused checks:

1. Two Macs, multiple account homes, identical imports, edits on either Mac,
   offline catch-up, incomplete iCloud downloads, interruption and retry.
2. Concurrent edits retain the working installation; explicit disable/removal
   cannot resurrect on the peer. Name collisions and unmanaged files survive.
3. Test credentials in environment values, headers, arguments and URLs never
   enter transport packages or diagnostics. Account bindings remain separate.
4. Path changes, missing runtimes, file permissions, escaping links and unsupported
   skill features yield correct installation or an actionable state.
5. Account/provider switching preserves managed MCP entries, local sign-ins and
   unrelated configuration. Existing chat sync retains its behavior.
6. Isolated official-client discovery confirms a synthetic skill and MCP setup;
   the Cowork plugin import is verified through its native UI.
7. A representative physical two-Mac iCloud transfer verifies the new paths before
   claiming end-to-end compatibility. Record any unavailable verification plainly.

Add the library suite to CI, update setup/limitations and the changelog, and ship
one feature PR after its checks pass. Use the existing Developer ID signing,
notarization and GitHub release workflow for the next build. Enabling this feature
must not silently enable chat sync or alter existing chat-sync settings.

## V1 boundaries

Repository-scoped configuration, arbitrary third-party plugin conversion,
provider-owned connector migration, credential synchronization, automatic runtime
installation, and unverified automatic Cowork updates are excluded. Installation
references for marketplace-managed plugins can be a later extension.

## Evidence and implementation checks

- [Codex skills](https://learn.chatgpt.com/docs/build-skills): local discovery,
  standard skill folders, supporting files, and plugin distribution.
- [Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp?surface=cli):
  native configuration, local/HTTP transports and authentication options.
- [Claude skill locations and Cowork](https://code.claude.com/docs/en/skills#use-skills-in-cowork-and-cloud-sessions):
  local skills differ from account-enabled Cowork skills; the synced directory
  is a download cache.
- [Claude plugin support](https://support.claude.com/en/articles/13837440-use-plugins-in-claude):
  account-based installation and component availability across Claude surfaces.
- [Claude connector authentication](https://code.claude.com/docs/en/mcp#use-mcp-servers-from-claudeai):
  provider-owned connections may not be portable to another client.

These documents were checked during planning. Milestone 1 verifies the installed
client versions and formats before implementation relies on them.
