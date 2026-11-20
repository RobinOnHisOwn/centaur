# Centaur terminal sessions

A thin extension of Centaur's existing Rust session CLI. Pi, tools and repositories run **remotely**; this executable only calls the session API and renders events. No local Kubernetes access, SSH, tmux, session database or agent subprocess is involved.

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
centaur login
```

Login prompts without echo, verifies the secret with `/oauth/token`, and saves it in `~/.config/centaur/config.json` with permissions **0600**. This is a protected local file, not encrypted storage. Use a protected device/account and disk encryption. `CENTAUR_CONFIG` overrides the path; otherwise `XDG_CONFIG_HOME` is honored. Atomic config replacement uses a mode-0600 temporary file beside the config so the rename stays on the same filesystem.

A secret manager can pipe the secret to `centaur login --secret-stdin`, or inject `CENTAUR_CLIENT_SECRET` without writing it to the config. Never put secrets in CLI arguments. Endpoint/client changes invalidate saved credentials; an environment endpoint override cannot silently reuse a credential saved for another endpoint. HTTP is accepted only on loopback for local tests; redirects are not followed.

Access tokens stay in memory. The client reacquires expiring tokens using the saved client secret, retries a rejected token once, and resumes SSE with the last event ID. Revoked credentials/scopes fail visibly instead of retrying forever. This is machine-client login, not a browser/user OAuth flow. Legacy `CENTAUR_API_TOKEN` is still accepted, but cannot be renewed; session discovery requires an API-client identity.

Optional `centaur config set principal <attached-principal>` selects another attached principal for new sessions. It does not alter an existing session's principal. `centaur config unset principal` restores default selection for new sessions.

Login verifies authentication only, not permission to create a session. The administrator must attach an approved principal to the API client and select a default (or supply an already attached principal explicitly). Personal integrations additionally require a Console-user principal linked to the client's creator; creating an arbitrary principal with your name is not equivalent. Do not borrow infrastructure identities or copy their grants. The CLI does not create principals or grant permissions.

### Inspect and undo local setup

```bash
centaur config show
centaur config unset principal
centaur logout
```

`config show` prints only allowed non-secret settings, their sources and credential presence; it makes no network requests. Endpoint precedence is `--api-url`, then `CENTAUR_API_URL`, then saved config. `CENTAUR_CLIENT_ID` and `CENTAUR_CLIENT_SECRET` override their saved values. `CENTAUR_API_TOKEN` selects legacy bearer authentication. Principal selection comes from saved config, not an environment variable.

`config unset endpoint` and `config unset client-id` also invalidate the saved credential. `logout` removes the saved secret and its endpoint/client binding, but keeps other settings. It does **not** revoke credentials on the server, stop work, or remove credentials injected through arguments/environment. No browser SSO or automatic principal provisioning is performed.

Use separate `CENTAUR_CONFIG` files for separate installations rather than repeatedly replacing credentials:

```bash
CENTAUR_CONFIG="$HOME/.config/centaur/alternate.json" centaur config show
```

### Legacy invocation compatibility

The original root execution flags still work when no subcommand is used. Mixing them with `chat`, `config`, `login`, or `integrations` now fails rather than silently ignoring them. For example, use `centaur chat new --harness pi`, **not** `centaur --harness-type pi chat new`. Connection flags remain usable before subcommands. The old root execution defaults remain unchanged; modern chat commands use their documented defaults.

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
