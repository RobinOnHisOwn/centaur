# Terminal workflow assessment

Source inspected: paradigmxyz/centaur `6e293ead961dcd7800a1c6dba456d6f8e5b1e9ee` (2026-11-12). This is upstream source evidence, not a claim about a deployed installation.

## Console administration addition (2026-11-20)

`admin` remains in this executable but uses a focused Console HTTP client in `src/admin.rs`, not the api-rs session client. Admin endpoint/key settings are separate, stored by the existing atomic protected config writer and bound to the admin destination. Chat/client credentials, browser cookies and sandbox tokens are never substituted. Administrative requests disable redirects and retries, bound input/response/list sizes, validate write documents and encode dotted foreign IDs before Rails format recognition.

The first delivery covers existing bearer operations: generic principals, roles, assignments, direct grants and redacted effective config. Users, API clients and principal deletion deliberately remain absent because those existing endpoints are Console-session-only. No server authentication namespace is widened. Role upserts retry a server-local uniqueness race once, and shared grant validation rejects ambiguous selector counts before resolving references; HTTP mutation retries remain disabled.

A future provider can use the underlying API; no provider framework, state database, bulk reconciler or credential provisioning system is introduced. See [README](README.md#console-administration), [plan](../../docs/architecture/plan-centaur-cli-administration.md) and [validation](VALIDATION.md#console-administration--2026-11-20).

## Reuse

- `crates/centaur-session-cli`: existing Rust CLI, Ratatui/Crossterm UI, and neutral user input. Extend it; Pi's local SDK/RPC client would start a local subprocess and is the wrong remote boundary.
- `centaur-api-server/src/routes.rs`: create/reuse, append messages, enqueue execution, interrupt, SSE replay, pause and sandbox release. `HarnessType::Pi` is accepted by the public create route; the Pi guide's “Slack only” statement is stale.
- `centaur-session-{runtime,sqlx}`: Postgres sessions, messages, executions, global event IDs, per-session serialization, ownership leases, queued-request adoption and backend output recovery. An SSE reader does not own the execution task.
- `harness-server/src/pi.rs`: one persistent RPC subprocess per assigned thread, deterministic session ID, native Pi persistence, neutral output events. The adapter intentionally buffers assistant text until `message_end` to suppress failed retry attempts. Token-by-token Pi streaming is not currently exposed by this boundary.
- `services/sandbox/entrypoint.sh`: independent workspace clone per sandbox, no shared writable Git worktree metadata. Repo cache is a seed, not the writable workspace. With a state volume, workspace, uploads, branches and `.pi` reside under the sandbox's state directory.
- Existing sandbox scheduling/warm pools, proxy identity, resource configuration and PVCs remain entirely server-owned. A conversation does not require an immortal Pod.
- API clients use OAuth client credentials, one-hour tokens, read/write scopes, attached principals and enforced `api:<client-id>:` namespaces. Client-supplied metadata cannot assert a requester identity. Revocation also terminates open SSE readers.

## Gaps and smallest change

Extend the existing executable and API client. Add authenticated, namespace-scoped discovery and durable state reads, without a local session registry/database. Use human names as the suffix of a namespaced thread key. Add configuration, credential exchange, lifecycle commands, replay/reconnect, multiline input and history to the current UI. Do not add controllers, SSH, tmux, direct Pod operations or a new runtime.

Sandbox release is destructive to its filesystem and native Pi memory, but retains the database transcript/events. It must not be mislabeled persisted-session deletion. The accompanying API now adds idle-session archive/unarchive using metadata and hard deletion using a session-row lock, bounded sandbox cleanup and existing SQL foreign-key cascades. Both retain the API-client namespace boundary; archive retains history/files while delete requires explicit CLI confirmation. No per-create repository/workspace/resource fields exist in `CreateSessionRequest`; use deployment configuration/personas and normal agent Git operations rather than pretend metadata provisions a repository.

## Authentication boundary

Centaur pins Pi 1.5.0. Its adapter supports only the allowlisted `anthropic`/`openai` API-key models; proxy placeholders supply keys. `centaur-api-server/src/args.rs` rejects subscription/access-token auth for the Pi default harness. Pi itself has account OAuth support, but that does not establish Centaur support: separate sandbox auth files cannot safely coordinate shared rotating refresh tokens, and a shared writable auth volume would cross isolation boundaries. Local Pi's file locking is not a distributed token broker. No account login, token renewal or concurrent subscription usage has been live-tested here.

The installed, matching Pi 1.5.0 source was also inspected: `pi-ai/dist/auth/oauth/openai-chatgpt.js` implements PKCE, a local/manual callback, offline-access refresh and direct API token sharing for the `openai` provider. `pi-ai/dist/providers/openai-codex.js` separately defines legacy ChatGPT backend access. Thus account authentication is not limited to `openai-codex`, but neither flow is wired into Centaur's Pi credential fragments. Provider concurrency/account-policy limits remain unknown, not a proven prohibition.

Smallest supported alternative: OpenAI API-key credentials through the existing iron-proxy configuration (`CODEX_AUTH_MODE=api_key`, deployment Pi model `openai/<supported-model>`). This is separately billed API authentication, not ChatGPT subscription authentication. CLI authentication to Centaur is separate and must not upload local Pi credentials.

## Recovery limits / prerequisites

`sandbox.stateVolume.enabled` defaults to **false**. Enable it with a suitable existing storage class for native Pi/filesystem recovery after Pod replacement. Final sandbox deletion removes that state. Postgres transcript durability alone cannot reconstruct Pi's complete native state or resume a tool midway through execution. Orphan adoption recovers queued requests and backend-recorded output where available; irrecoverable running work becomes a visible failure, not an automatically replayed side effect.

No Centaur endpoint or API-client credentials were present in this shell. Kubernetes mutation is not authorized in this environment. Live acceptance (disconnect during a real Pi task, concurrent sandbox execution, Pod replacement, proxy credentials and model access) remains blocked on a configured deployment and must be reported separately from local contract tests.
