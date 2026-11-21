# Centaur terminal CLI

A thin HTTP client for remote chats and explicit Console administration. Pi, tools and repositories run **remotely**. Chat commands use api-rs; `admin` uses Console's separate operator API and credentials. No local Kubernetes access, SSH, tmux, session database or agent subprocess is involved.

## Server prerequisites

Deploy the accompanying api-rs changes first:

- `GET /api/sessions`: API-client-only discovery, restricted to the authenticated `api:<client-id>:` namespace. Pages contain at most 100 sessions; `after_thread_key` selects the next page. The client cannot choose another namespace.
- `GET /api/session/{thread_key}/state`: existing session authorization; returns persisted session, current/latest execution, submitted messages and latest event ID. Reads do not create a sandbox or execution.
- Existing create/messages/execute/interrupt/events/sandbox endpoints are unchanged. No database migration is needed.

Enable `apiClients.enabled`, create an API client in Console with `sessions:read` and `sessions:write`, and attach an appropriate default principal. Expose `/oauth/token`, `/api/sessions` and `/api/session/` over HTTPS, not the administrative API. Ingress/proxy buffering must be disabled for SSE.

For filesystem/native Pi recovery, enable `sandbox.stateVolume.enabled` using the existing storage settings. It defaults to **false**. Resource limits are deployment-owned under `sandbox.resources`; warm pools, capacity and idle cleanup remain server-owned. No per-session resource or repository provisioning fields exist in the current create API, so the CLI does not invent them.

Set up the sandbox's existing repo-cache/`AGENT_REPO` configuration and principal Git grants normally. Each sandbox gets its own writable clone and branch. Repositories can also be cloned through ordinary agent tool calls under that principal's egress policy. Do not use a shared writable mount for Git checkouts or Pi auth/session files; do not push multiple agents to the same Git branch.

## Build / install on macOS

From the repository root (Rust toolchain required):

```bash
cargo build --locked --release -p centaur-session-cli
```

Install the existing crate's executable under the shorter command name:

```bash
install -m 755 target/release/centaur-session-cli /usr/local/bin/centaur
```

Choose a user-writable directory on PATH instead when `/usr/local/bin` needs elevated access. The installed executable does not require the source checkout, Rust, Pi, or kubectl.

## Configure and log in

Set `CENTAUR_API_URL` and `CENTAUR_CLIENT_ID` to the endpoint and client ID provided by your administrator, then:

```bash
centaur config set endpoint "$CENTAUR_API_URL"
```

```bash
centaur config set client-id "$CENTAUR_CLIENT_ID"
```

```bash
centaur login --auth-mode machine
```

Login prompts without echo, verifies the secret with `/oauth/token`, and saves it in `~/.config/centaur/config.json` with permissions **0600**. This is a protected local file, not encrypted storage. Use a protected device/account and disk encryption. `CENTAUR_CONFIG` overrides the path; otherwise `XDG_CONFIG_HOME` is honored. Atomic config replacement uses a mode-0600 temporary file beside the config so the rename stays on the same filesystem.

A secret manager can pipe the secret to `centaur login --secret-stdin`, or inject `CENTAUR_CLIENT_SECRET` without writing it to the config. Never put secrets in CLI arguments. Endpoint/client changes invalidate saved credentials; an environment endpoint override cannot silently reuse a credential saved for another endpoint. HTTP is accepted only on loopback for local tests; redirects are not followed.

Access tokens stay in memory. The client reacquires expiring tokens using the saved client secret, retries a rejected token once, and resumes SSE with the last event ID. Revoked credentials/scopes fail visibly instead of retrying forever. This is the machine-client path, retained for automation and compatibility. Legacy `CENTAUR_API_TOKEN` is still accepted, but cannot be renewed; session discovery requires an API-client identity.

### Browser login for people

For the interactive human CLI, use the Console HTTPS URL instead of configuring a machine client:

```bash
centaur login --server https://console.example.com
centaur auth status
centaur chat new --name project --message "Review the project"
```

The CLI opens Console sign-in and receives a short-lived code through a loopback callback. It stores the rotating refresh credential in the same mode-0600 config file. The browser login grants ordinary personal chat/integration access; Console administrators may separately consent to CLI administration. Admin actions still check current administrator status. Use `centaur auth connections list` and `centaur auth connections revoke <UUID>` to manage other devices. `centaur logout` revokes this connection and removes the local browser credential; it does not cancel accepted work or other connections.

First chat creation prepares/links the personal Console principal for the signed-in user. This is an explicit account-bound operation, not a generic admin action; ambiguous or conflicting existing principals require Console operator reconciliation. Existing machine credentials remain available with `--auth-mode machine`; operator-key administration remains available with `--auth-mode operator`. These identities never silently override the browser identity. The CLI browser protocol is fork-specific and must be enabled/configured by the Console/API operator. In the Helm chart, set `cliAuth.enabled=true`, `cliAuth.sessionResource` to the externally reachable HTTPS api-rs resource URL (ending in `/api/cli/v1`), and `console.publicUrl` to the canonical HTTPS Console URL. Create the Secrets named by `cliAuth.signingKeySecretName` and `cliAuth.verificationKeysSecretName`; the first must contain `private.pem` as a PEM RSA private key, and the second `keys.json` as `{ "cli-v1": { "n": "<base64url modulus>", "e": "AQAB" } }`. Keep the private key only in the Console Secret; api-rs receives only the public key. Provision both Secrets using the deployment's existing secret manager, never Helm values or Git plaintext. api-rs also needs read access to Console `users`, `principals`, and `cli_oauth_refresh_tokens` in its configured Console database. The Console migration creates OAuth tables. The browser endpoints stay disabled unless `cliAuth.enabled` is set.

Optional `centaur config set principal <attached-principal>` selects another attached principal for new sessions. It does not alter an existing session's principal. `centaur config unset principal` restores default selection for new sessions.

Login verifies authentication only, not permission to create a session. The administrator must attach an approved principal to the API client and select a default (or supply an already attached principal explicitly). Personal integrations additionally require a Console-user principal linked to the client's creator; creating an arbitrary principal with your name is not equivalent. Do not borrow infrastructure identities or copy their grants. Chat login never creates principals or grants permissions. Explicit operator operations are available separately under [`admin`](#console-administration).

### Inspect and undo local setup

```bash
centaur config show
centaur config unset principal
centaur logout
```

`config show` prints only allowed non-secret settings, their sources and credential presence; it makes no network requests. Endpoint precedence is `--api-url`, then `CENTAUR_API_URL`, then saved config. `CENTAUR_CLIENT_ID` and `CENTAUR_CLIENT_SECRET` override their saved values. `CENTAUR_API_TOKEN` selects legacy bearer authentication. Principal selection comes from saved config, not an environment variable.

`config unset endpoint` and `config unset client-id` also invalidate the saved credential. Machine-mode `logout` removes the saved secret and its endpoint/client binding, but keeps other settings. It does **not** revoke machine credentials on the server, stop work, or remove credentials injected through arguments/environment. Browser-mode logout revokes its CLI connection.

Use separate `CENTAUR_CONFIG` files for separate installations rather than repeatedly replacing credentials:

```bash
CENTAUR_CONFIG="$HOME/.config/centaur/alternate.json" centaur config show
```

### Legacy invocation compatibility

The original root execution flags still work when no subcommand is used. Mixing them with `chat`, `config`, `login`, or `integrations` now fails rather than silently ignoring them. For example, use `centaur chat new --harness pi`, **not** `centaur --harness-type pi chat new`. Chat connection flags remain usable before chat/config/login/integrations subcommands. Explicit root `--api-url`/`--api-token` flags are rejected with `admin`, which has separate settings. The old root execution defaults remain unchanged; modern chat commands use their documented defaults.

## Console administration

Use `centaur admin` with the consented browser identity when available, or explicitly select `--auth-mode operator` for an authorized Console **operator API key**, whose owner must remain an active administrator. This is not a chat API-client secret, Keycloak token, sandbox token or Console browser cookie. The Console operator endpoint must be reachable through your deployment's existing administrative access policy; these commands do not expose it publicly or change ingress.

```bash
centaur admin config set endpoint https://console.example.com
centaur admin login --auth-mode operator
centaur admin status --auth-mode operator --json
centaur admin config show
```

Login prompts without echo. A secret manager can instead pipe the key to `centaur admin login --key-stdin`; never pass it in arguments. It verifies `GET /api/v1/roles?limit=1&page=1` before saving the key in the existing mode-0600 config, bound to that exact admin endpoint. This proves current operator authorization, not session readiness or least privilege. Config and logout commands do not revoke server credentials.

For automation, inject `CENTAUR_ADMIN_URL` and `CENTAUR_ADMIN_API_KEY` from a secret manager. These override saved admin settings; there is no chat-endpoint or chat-token fallback. Environment-only use does not save a key unless `admin login` is explicitly run. Admin logout removes only the saved admin key/binding; root logout remains chat-only. Both preserve the other credential type. `admin config unset endpoint` removes the saved admin endpoint and bound key; changing it invalidates the saved key. Environment overrides remain active.

### Available commands

All entries below are relative to `centaur admin`; append `--json` for one machine-readable JSON object (diagnostics remain on stderr). The human default is pretty JSON.

| Group | Operations |
| --- | --- |
| `principals` | `list`, `get <ref>`, `create`, `update <ref>`, `upsert <foreign-id>`, `effective-config <ref>` |
| `principals roles` | `list <principal-ref>`, `add <principal-ref> <role-ref>`, `remove <principal-ref> <role-ref> --yes` |
| `roles` | `list`, `get <ref>`, `create`, `update <ref>`, `upsert <foreign-id>`, `delete <ref> --yes` |
| `grants` | `list --principal <ref>` or `list --role <ref>`, `get <grant-oid>`, `create`, `delete <grant-oid> --yes` |

A ref is an exact resource OID or URL-safe foreign ID, never a display name. Reference length is limited to 256 bytes. Dots are encoded in requests so Rails does not mistake foreign IDs for format suffixes. Grants and their secret references use OIDs only.

`list` aggregates every page as `{"items":[...]}`; `--limit 1..200` changes page size, not the total result count. Principal/role lists also accept repeated `--label KEY=VALUE` filters. Resource reads/writes return `{"data":...}`; successful removals return `{"ok":true}`. No partial JSON is printed if a page fails. Lists are not transactional snapshots and stop with an explicit error above 8 MiB or 10,000 pages; individual input/response documents are limited to 1 MiB. Requests have a 5-second connect and 30-second total timeout. Redirects are never followed, and HTTP is allowed only on loopback.

### Provisioning from JSON

**Before creating a principal, review the deployment's default roles, sandbox capabilities and credential reconciliation.** A generic principal can inherit powerful authority even when its input contains no explicit grant. Do not borrow infrastructure identities or describe this as automatic least-privilege provisioning. Creating a principal is not creating or linking a Console user.

Creation and updates take `--file PATH` or `--file -` (stdin), with the existing `data` envelope:

```bash
printf '%s\n' '{"data":{"name":"Analysts","labels":{"team":"analytics"}}}' \
  | centaur admin roles upsert analysts --file - --json
printf '%s\n' '{"data":{"name":"Analytics worker","sandbox_repo_cache":"none"}}' \
  | centaur admin principals upsert analytics-worker --file - --json
centaur admin principals roles list analytics-worker
centaur admin principals roles add analytics-worker analysts
centaur admin principals effective-config analytics-worker
```

`create` fails on duplicates. `update` requires an existing resource and resolves foreign IDs to OIDs before PATCH, so typos cannot create records. `upsert` sends a direct server-side PUT by foreign ID; repeated calls retain the resource OID. Neither update nor upsert revokes omitted role assignments or grants. Supplied labels replace the label object; omitted fields are retained. Only create/upsert may include `foreign_id`, which must match the upsert argument.

Role documents support `name`, `labels` and optional create/upsert `foreign_id`. Principal documents additionally support `sandbox_repo_cache` (`none`, `public`, `all`) and boolean `sandbox_observability_enabled`, `sandbox_sessions_read_enabled`, `sandbox_workflows_read_enabled`, `sandbox_workflows_write_enabled`. Unknown fields, account-linking/Slack identity fields and global default-role policy are rejected; they are not provisioned implicitly. Native Console personal-principal provisioning remains separate.

To grant an existing secret to a role, substitute OIDs from your authorized Console inventory:

```bash
printf '%s\n' '{"data":{"role_id":"role_RETURNED_ID","static_secret_id":"ssr_EXISTING_ID"}}' \
  | centaur admin grants create --file - --json
centaur admin grants list --role analysts
```

A grant document requires exactly one `principal_id` or `role_id`, plus exactly one of `static_secret_id`, `gcp_auth_secret_id`, `gcp_id_token_secret_id`, `aws_auth_secret_id`, `oauth_token_secret_id`, `pg_dsn_secret_id`, `hmac_secret_id`, `ssh_key_secret_id`. Secret values are never requested. Repeating an identical grant/assignment is idempotent on the server. Grant lists show **direct** grants; effective config is server-redacted, not an upstream authorization test.

Role deletion, assignment removal and grant deletion require `--yes`; without it no request is sent. Deleting a role removes all its grants/assignments, not principals or secrets. Removing one grant does not revoke access supplied by another inherited/direct grant. No mutation is automatically retried after transport failure: inspect `get`/`list` before resubmitting because the server may already have committed. Error response bodies are deliberately omitted because they can contain credentials.

**Not available:** users, API clients and principal deletion. Their existing endpoints deliberately require Console sessions; operator keys do not gain that authority. No cookie scraping, new administrative scopes, secret creation, Terraform provider or declarative reconciler is included. See the [administration plan](../../docs/architecture/plan-centaur-cli-administration.md) for the unresolved authentication gate.

## External integrations (requires matching Console and API versions)

The source of truth is **Console → Integrations**, not a local or session-database registry. An operator must enable a non-shared custom OAuth app (or Granola) with the label `mcp=true` (string value). Use a credential-free HTTPS resource URL and configure Console's public HTTPS origin. Expose the scoped `/api/mcp/servers` routes to CLI users, not the Console operator API.

- `centaur integrations list` shows registered apps, account status and whether the personal principal has a grant. These are not connectivity checks.
- `centaur integrations connect docs` prints Console's consent link. Complete consent in a browser with your own Console account; the command does not confirm completion.
- `centaur mcp list/login` remain compatibility aliases. `centaur mcp add docs --url https://mcp.example.com/mcp` is deprecated: it only validates an existing registration and prints consent; it does not register a new endpoint.
- These commands configure external integrations **used by** remote agents, not an MCP server for controlling Centaur. This CLI does not yet provide the planned `mcp serve` session adapter.
- New Codex sessions snapshot eligible endpoints; existing sessions retain their endpoint list. An API client needs a matching personal default principal. No new token store, implicit grant, local provider login, or database migration is needed.
- Disconnect only through the signed-in Console. This removes **all** of your saved accounts for that integration and their wrapper grants, not just a CLI setting. It does not revoke tokens at the provider. There is no preference-only disable command.
- The provider or MCP server enforces allowed operations. A proxy grant does not imply read-only access. Verify the actual Codex/proxy path, overlapping credential rules, refresh ownership and revocation before rollout.

## Daily workflow

New chats default to Codex; no harness flag is needed. Use `--harness pi` only
when explicitly choosing Pi with its separate provider authentication configured.

```bash
centaur chat new --name dbt-refactor
```

The UI uses Pi-inspired Catppuccin Mocha colors, shaded user/tool panels, Markdown replies and a compact editor with purple horizontal borders. A persistent **CENTAUR** header identifies the remote session and harness. Provider failures appear inline in red; **F2** still exposes raw events. Markdown uses the pinned `tui-markdown` Ratatui adapter without its optional syntax-highlighting bundle.

In the UI: **Enter** sends; **Alt-Enter** adds a newline; bracketed paste preserves multiline text; **Up/Down** recalls prompts, including persisted prompts after attachment; **PageUp/PageDown** scrolls the wrapped transcript; **F2** shows raw events. **Left/Right** move the insertion cursor, **Home/End** move to the current line boundaries, and **Backspace/Delete** remove adjacent Unicode scalar values. Paste inserts at the cursor. This is a small UTF-8-safe editor, not Pi's full editor or a grapheme-aware text editor.

**Ctrl-C** or `/interrupt` explicitly interrupts remote execution. **Ctrl-D**, `/quit`, terminal close or client crash only detach; they do not send cancellation. Until submission is acknowledged, a network failure is ambiguous—check status/history before resubmitting. Do not detach while the UI still says “sending message”.

```bash
centaur chat attach dbt-refactor
```

Attachment reads existing state and submitted prompts, then replays retained agent events and follows live output. Prompts and agent output are shown in separate sections, not a reconstructed chronological Pi transcript. Event IDs suppress duplicates during automatic reconnect. A new attachment replays from zero to rebuild the display; this intentionally redisplays history, not duplicate events inside one stream. The display retains 1,000 entries; `logs` exposes all retained events. Server retention may have removed older stdout events.

Run another independent session in another terminal, or submit without attaching:

```bash
centaur chat new --name snowflake-costs --detach --message 'Inspect the repository and summarize cost hotspots.'
```

```bash
centaur chat list
```

```bash
centaur chat status dbt-refactor
```

```bash
centaur chat history dbt-refactor
```

```bash
centaur chat logs dbt-refactor
```

```bash
centaur chat logs dbt-refactor --follow
```

```bash
centaur chat interrupt dbt-refactor
```

Names contain 1–64 ASCII letters, digits, hyphens or underscores. They map directly to `api:<client-id>:<name>`, with no local registry. `new` follows Centaur's create-or-reuse semantics; reusing a name/harness reuses that conversation. Omit `--name` for a new UUID name. `--persona` uses the existing persona mechanism; a persona may pin a different harness/model.

`status` includes actionable execution errors. `list` reports queued/running/completed/failed/cancelled, or idle if no execution exists. Commands do not hold tasks open: `chat send` and `new --detach --message` exit after durable enqueue. Tasks default to a one-hour server execution deadline; pass a positive `--max-duration-ms` on `new`/`send` to change it. `attach --max-duration-ms` sets the deadline for subsequent turns submitted from that attachment, not an existing execution or persistent session setting. No per-turn idle-pause timer is requested by normal chat messages; global server cleanup/capacity policy still applies.

For scripted multiline prompts, pass one quoted argument to `chat send`. It prints a request ID before submission. Reuse that exact ID and prompt with `--request-id` after an ambiguous failure; message append and execution are separate durable operations, not one transaction. They use matching deduplication/idempotency keys. Do not reuse an ID for different text. Ordinary POST transport failures are not blindly retried. Existing Centaur behavior may steer a message appended during an active execution; avoid simultaneous writers to the **same** session. Independent sessions can execute concurrently.

### Submission receipts and JSON output

A successful `send` or `new --message` reports the server's execution ID, status and request ID. This is an acceptance receipt, **not** the final answer. The TUI also displays these identifiers after submission. A request ID is printed on stderr before submission so ambiguous failures can be retried safely.

```bash
centaur chat send dbt-refactor 'Summarize the changes' --json
centaur chat new --name analysis --message 'Inspect the repository' --detach --json
centaur chat list --json
centaur chat status dbt-refactor --json
```

Receipt JSON has `ok`, `execution_id`, `thread_key`, `status`, and `request_id`. `new --json` requires both `--detach` and `--message`, preventing terminal UI output from corrupting JSON. `list --json` returns one object with an `items` array aggregated across pages; errors do not emit a partial result. `status --json` returns compact server state (the default remains pretty JSON). Diagnostics remain on stderr and failures return a nonzero exit code. `logs` remains JSONL.

`history` prints persisted messages, not a complete chronological transcript. Inspect retained agent events with `logs`. Existing client-scoped sessions and the separate append/enqueue protocol remain unchanged; this release does not enable shared CLI/MCP sessions, atomic submission or execution-targeted interruption.

## Lifecycle boundaries

- **Interrupt** ends the active execution. Database history and sandbox/native conversation state are retained where available; interrupted tools are not automatically resumed.
- **Detach** closes only the terminal reader. Running execution remains server-owned.
- **Release** calls Centaur's existing `DELETE /sandbox`. It is refused while busy and destroys filesystem/native Pi state, but retains database messages/events. A later turn gets a fresh sandbox, not the old native Pi conversation:

```bash
centaur chat release dbt-refactor --yes
```

- **Archive** hides an idle session from the default list without stopping its sandbox or deleting history/files. `chat archive dbt-refactor` archives it; `chat list --all` includes archived entries; `chat unarchive dbt-refactor` restores visibility. Archive is not an access-control boundary: exact-name reads and writes remain available.
- **Prune** previews all your inactive sessions whose most recent activity is strictly older than 30 × 24 hours: `chat prune`. This includes archived sessions, but never queued/running or workflow-owned ones. `chat prune --yes` permanently deletes eligible sessions using the same sandbox/history cleanup as delete. Age uses the server clock and the latest session change, sandbox activity, message, event or execution timestamp; recent archive/unarchive changes therefore reset the clock. Candidates are paginated and rechecked under a row lock immediately before cleanup, so activity after preview prevents deletion. Output reports matched/deleted/skipped/failed totals, with a nonzero exit on failures or an incomplete scan. Cleanup is sequential and may partially complete; there is no batch rollback. Unsupported backends are rejected rather than falling back to unconditional deletion.
- **Delete** permanently removes an idle session, its messages, events and executions, and its sandbox/native filesystem state: `chat delete dbt-refactor --yes`. It disappears from both list modes; its old name can subsequently create a new conversation. This does not erase external logs, backups or account credentials. Without `--yes` no deletion request is sent.
- Archive, unarchive and delete refuse queued/running executions and workflow-owned sessions. Delete holds a database row lock while stopping the sandbox (90-second cleanup deadline), then commits the existing foreign-key cascades. If cleanup fails, the session/history and sandbox reference are retained for retry, although partial sandbox cleanup may already have happened. Already-open event streams close on their next empty fetch/safety poll after deletion.
- These operations require the updated API server and CLI; the old production deployment does not support them yet. No database migration is needed; archive uses existing session metadata.
- **Sandbox pause** is an existing operator capability, distinct from interruption/release; it is not exposed to ordinary API-client credentials by this CLI.

## Model authentication: ChatGPT is not an API key

Pi 1.5.0 itself supports ChatGPT OAuth (including direct `openai` subscription auth and the legacy `openai-codex` provider). Native Pi can store and refresh those credentials. **Centaur's Pi integration does not currently broker them**: its supported credential fragments inject API-key placeholders, its model allowlist excludes `openai-codex`, and the Pi default-harness access-token mode is rejected.

Do not upload local `~/.pi/agent/auth.json`, copy rotating refresh tokens into separate sandbox files, or introduce a shared writable auth volume. Existing local file locks are not coordination between independent sandbox files. Interactive account login, refresh across sandboxes and provider concurrency limits have not been validated; no guarantee about unattended ChatGPT-account execution is made here.

The supported minimal alternative is an **OpenAI API key** through the existing iron-proxy credential configuration, `CODEX_AUTH_MODE=api_key`, and a supported `CENTAUR_PI_MODEL` such as `openai/gpt-5.5`. This is separately billed API usage, not the ChatGPT subscription. Model keys never go through terminal-client login/config.

## Validation and remaining acceptance work

See [ARCHITECTURE.md](ARCHITECTURE.md) for the source assessment and [VALIDATION.md](VALIDATION.md) for exact local evidence and limitations.

Unit/HTTP tests cover command parsing, UI input/output, OAuth rejection/renewal, replay deduplication and reconnect cursors. Database-backed tests exercise actual api-rs Pi session creation/state and namespace-scoped, paginated SQL discovery, alongside upstream execution/recovery tests.

An opt-in deployment acceptance test starts two real Pi tasks, kills their local stream consumers, waits for remote completion, checks actual tool output and isolated filesystem contents, and continues each conversation:

```bash
python3 crates/centaur-session-cli/tests/live_workflow.py --cli centaur
```

It spends model tokens and leaves two `accept-*` sessions for inspection. It does not simulate physical laptop failure or restart Pods. A real deployment/model credential is required. Pod replacement must be verified separately by an authorized operator; a lost running tool is not automatically replayed. No live deployment acceptance is claimed without that evidence.
