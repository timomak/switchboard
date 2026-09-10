# Saved Codex connections

**Manage accounts → Connections → Add connection** saves a user-named connection
for selection in the Codex Desktop menu. Choose a backend template, enter a model/deployment ID if
known, customize the source variable names, and select an existing dotenv file.
Secret values remain in that file; do not paste them into the form.

The UI renders templates supplied by Rust. Supported connection formats are
Azure OpenAI, Amazon Bedrock, and HTTPS OpenAI-compatible Responses APIs. These
are protocol adapters, not accounts hard-coded into the application. No account
is created on a fresh installation until the user adds one.

```sh
ai-usagebar codex-provider add team-gateway --provider openai-compatible \
  --source /absolute/path/to/.env.local \
  --credential-key TEAM_API_KEY --endpoint-key TEAM_API_URL \
  --model your-model-id
ai-usagebar codex-provider list
ai-usagebar codex-provider verify team-gateway --json
```

`--credential-key`, `--endpoint-key`, and `--region-key` name fields in the source
file. New Azure connections default to `AZURE_OPENAI_API_KEY` and
`AZURE_OPENAI_ENDPOINT`. Bedrock defaults to `AWS_BEARER_TOKEN_BEDROCK` and
`AWS_REGION`. OpenAI-compatible connections default to `OPENAI_API_KEY` and
`OPENAI_BASE_URL`. Existing Azure records retain their earlier field mappings;
they are not silently rebound to another credential.

## Model selection and access

There is no baked-in model catalog or allowlist. The UI displays only the
connection's explicitly configured model/deployment ID, or **No model selected**.
A configured ID is user input, not a claim of account access. Azure expects a
resource-specific deployment name. Bedrock and compatible APIs expect their
provider-specific model IDs.

**Check configuration** reads only selected literal fields, validates endpoint
and region syntax, serializes a proposed configuration, and inspects local login
constraints and workflow model use. It does not authenticate, discover models,
run inference, or verify model permissions. Account-specific discovery would
need a separate provider-backed operation with explicit credential use; public
catalogs must never be presented as that result.

## Selecting a connection

Use **Use connection** on a saved entry or choose it in the Codex Desktop
selector. Enter the organization-specific model/deployment ID when asked.
Confirm the restart after finishing active Codex tasks and CLI sessions.

```sh
ai-usagebar codex-provider use team-gateway --model your-model-id --yes
ai-usagebar codex-provider subscription --yes
ai-usagebar codex-provider recover --yes
```

Selection updates only `config.toml` and `.env` in the existing CODEX_HOME.
It reads the selected source fields on demand and writes the necessary runtime
environment fields privately for the official app. It never passes credentials
as command arguments or prints them. Azure/custom connections bind their named
API-key variable; Bedrock mappings become the AWS runtime token and region.
It preserves unrelated config and removes first-party login/effort/service-tier
overrides while the provider is selected. Selecting a saved subscription restores the
original connection configuration and environment, including whether `.env` existed.
Unrelated settings changed by Codex while connected, such as plugin settings, are retained.
The source file is never edited and the OAuth auth file is never replaced.

Connection metadata and private before/after recovery records are stored under
`~/.claude-acc/codex-cloud-providers/`. Per-operation backups and a pending journal
allow rollback after partial writes. Outside edits to connection-owned fields or
the environment are detected; the switcher refuses to overwrite them. Unrelated
config changes do not invalidate the selection. Resolve connection edits manually instead of
forcing a whole-file rollback. Recovery never rewrites threads, pins, projects,
worktrees, or workflow definitions. The older `stage` registration command remains
an alias of `add` for compatibility.

Codex quits gracefully before changes. Managed CLI sessions block selection while
running; the selected home's daemon is stopped before configuration is written.
Recovery and return also restart Codex. No model request is made by the switcher,
but the official app and scheduled work can contact the provider after restart.
Model IDs in scheduled workflows and project-level overrides must be compatible
with the selected provider. Preserved workflow files are not a guarantee of
successful runs under another provider. Existing workspace and historical thread
provider metadata stay in place.

The UI shows the selected connection and hides subscription quota for it. It has
no hard-coded cloud names or model catalog; provider-format templates come from
the backend and connection names/models come from user configuration.

## Validation

Hermetic Rust tests cover legacy mapping compatibility, custom adapters and field
names, arbitrary model IDs without a catalog, secret redaction, invalid literal
fields and endpoints, and unchanged fixture auth/history/pins/workflows. Activation tests cover full config restoration, interrupted-write recovery, external-edit refusal, and missing-model rejection.
Swift checks cover metadata parsing and no usage requests for inactive entries.
No user credential or live home is used by automated tests.

## References

- [Codex provider configuration](https://learn.chatgpt.com/docs/config-file/config-advanced)
- [Bedrock configuration and feature limits](https://learn.chatgpt.com/docs/amazon-bedrock)

Select a saved subscription directly from the Desktop menu while using a connection.
The equivalent CLI command is `ai-usagebar codex-provider subscription --account LABEL --yes`.
It validates the saved login before quitting, restores subscription configuration,
then uses the existing transactional account switch and verification before reopening.
If account verification fails, the previous subscription login is retained; an
unresolved authentication journal keeps the desktop closed for recovery.

## Claude Bedrock connection

Register a connection for both Claude selectors:

```sh
ai-usagebar cli claude-bedrock NAME --source /absolute/path/to/source.env --model YOUR_BEDROCK_MODEL_ID
```

The source must contain `AWS_BEARER_TOKEN_BEDROCK` and `AWS_REGION`.
Only one Claude Bedrock connection is currently supported. Registration reads and
validates these fields but stores only the source path, model, and label. It does
not select either surface or copy a key.

The Desktop option uses Claude's native `Claude-3p/configLibrary` configuration
and a private credential helper, with a backup of the previous configuration
selection. Claude's third-party mode has its own local workspace: subscription
chats and routines stay in the subscription workspace. The Subscription entry
restores the prior configuration selection. Neither route rewrites OAuth tokens.
The official app may offer a first-use screen to enter third-party mode.

The CLI option applies to `ai-usagebar cli launch claude` (the arrow button), not
plain `claude` launched elsewhere. It injects credentials into the child process
only and pins the primary and background model aliases to the configured model.
Selecting Subscription removes this launcher selection. Existing CLI sessions
must finish before changing selection; Claude Desktop and Codex are unaffected.

## Saved tasks bind to provider identities

A task created under a configured provider can continue to reference that provider after the default changes. Connection use, subscription return and recovery now refuse to remove/rebind definitions referenced by the current home's task DBs or rollouts, or to change their runtime credential environment. This prevents missing-provider errors and reuse of `custom` for a different endpoint. It can deliberately block subscription return after a provider task has been created. No task metadata is automatically migrated. See the [compatibility policy and future stable-identity design](codex-task-provider-compatibility.md).
