---
title: Centaur session orchestration over MCP
prd: "Not applicable: direct user requirements"
date: 2026-11-20
author: Pi
status: Draft
---

# Plan: Centaur Session MCP Implementation

## Source

The user requested implementation after reviewing the full eight-phase plan on branch `docs/session-mcp-plan`. On 2026-11-20 they explicitly selected **shared access** from Pi, CLI and ChatGPT, and **immediate client access revocation without cancellation of accepted work**. These decisions supersede the draft's separate namespaces and expiry-only revocation. Baseline: `915033a32b62552bc94590059cc667295401bd1d`.

Implementation belongs in this isolated feature worktree. The subsequent review/stage/commit request authorizes a local feature-branch commit. Push, public exposure, deployment and real model workloads remain unauthorized. No additional service container, database or queue is planned.

## Architecture Overview

- Local Pi launches `centaur mcp serve` over stdio. The CLI reuses its protected configuration and authenticated Centaur client; no model credentials are copied.
- Hosted clients use the existing `/mcp` in api-rs with Console OAuth.
- Both adapters share the same tool contract and durable session operations. PostgreSQL and the existing runtime own message/execution state, queued recovery, sandbox assignment and output.
- Shared ownership is based on a verified personal Console identity, never email or a caller-supplied principal. An API client qualifies only when its active creator and explicitly attached personal default principal match (existing `personal_mcp_identity` policy). Browser tokens resolve the same active personal principal. New shared sessions use a server-owned namespace; old machine namespaces remain isolated and are not migrated automatically.
- OAuth access tokens for delegation must identify a durable connection. Refresh rotation preserves that connection; revoke disables it immediately on subsequent calls. Disconnect revokes access, not already-accepted work. A client's local removal of a saved credential cannot be detected unless it invokes revocation; provide an explicit server-side revocation operation and document that distinction.

## Components

### Contract

Six tools: `centaur_session_submit`, `centaur_session_list`, `centaur_session_status`, `centaur_session_result`, `centaur_session_send`, `centaur_session_interrupt`. Arguments reject unknown fields and caller-controlled identity. Submit/send require a stable request ID; result and interrupt require the intended execution ID. Reads are bounded and read-only annotated; writes are not read-only. Results include structured content and a text compatibility representation. No destructive delete/prune/release, arbitrary shell or credential tools.

Defaults proposed by the original plan: 64-KiB prompt, 256-KiB protocol request, 64-KiB response, pages default 50/max 100, one-hour maximum execution, four active delegated executions and twenty new submissions/minute/owner. These are server limits, not a claim of a monetary budget.

### Identity and revocation

Reuse Console identity and principal bindings. Preserve `mcp:tools` defaults and add explicit session read/write consent. Existing catalog clients do not gain session authority automatically. Check current identity, connection and scope before list/read/write. A revoked connection cannot refresh or use an unexpired access token; another connection for the same user still works. Disabling the user blocks all connections. Never use global revocation merely to remove one client.

The active shared session is independent of the connection that created it. A newly authorized connection for the same personal owner can rediscover it. Legacy API-client sessions retain their original authorization boundary. Clarify human CLI operations needed to attach to new shared sessions without broadening legacy routes to unrelated sessions.

### Persistence and lifecycle

Atomic message/enqueue with request-payload collision detection. Do not call ordinary append's active-execution steering path. Dispatch only after commit, using existing driver/adoption. Replaying identical requests returns the original receipt. Changed payload under the same request ID is a conflict. Follow-ups reject busy sessions rather than implicitly steer. Test mixed legacy/new admission races.

Status/results select the execution, not current/latest state. Output is bounded, paginated and explicitly retained-only where completeness cannot be proven. Oversized-event paging must progress. Interrupt targets the expected execution through delivery; it must never hit a successor. Client EOF, transport cancellation or timeout does not cancel accepted work.

### Protocol and packaging

CLI stdout is only newline-delimited MCP JSON; diagnostics use stderr. Implement initialize, initialized, ping, list and call with bounded parsing, supported-version negotiation and safe errors. No interactive login during stdio service startup. Hosted Streamable HTTP remains in `/mcp`, returns 202 for accepted notifications, rejects invalid Origin/version headers, and may return GET 405. No experimental MCP Tasks or new SDK/framework is required unless actual conformance evidence rules out reuse.

## Implementation Order

| Phase | Component | Dependencies | Estimated Scope |
|---|---|---|---|
| 1 | Revised contracts, shared ownership and development preflight | None | S |
| 2 | Connection lifecycle, explicit scopes and shared authorization | Phase 1 | L |
| 3 | Atomic submit and owner-scoped admission | Phase 2 | L |
| 4 | Execution-specific results and targeted interrupt | Phase 3 | M |
| 5 | Shared server application functions and client methods | Phases 2–4 | M |
| 6 | CLI stdio and hosted adapters | Phase 5 | M |
| 7 | Cross-transport security/recovery tests | Phase 6 | M |
| 8 | Feature flags, documentation, CI and acceptance gates | Phase 7 | M |

### Task 1: Preflight and contracts

Inspect live repository state, relevant AGENTS files, actual Console schema, runtime/store paths and existing test fixtures. Use an isolated worktree. Obtain pinned Rust/Ruby tools and a disposable local database; no production credential reads. Establish baseline CLI and OAuth tests before changes. Read the original plan for unchanged detailed acceptance cases. Record changed identity/revocation decisions here.

### Task 2: Identity and connections

Files: Console OAuth controller/models/routes/views and generated migration/schema; Console OAuth tests; `centaur-console-db` identity/connection reads; api-rs MCP claims/authorization. Write failing tests first for stable connection across refresh, individual revoke, another connection still usable, user disable, scope escalation and wrong-user/principal bindings. Generate migrations with Rails; never hand-edit schema.rb. Keep legacy catalog OAuth compatible. Test shared API-client binding using the existing explicit attachment/creator rule rather than adding implicit email matching.

### Task 3: Atomic submission

Files: session-core types, session-sqlx store, session-runtime. Add tests for duplicate/changed/parallel request keys, name collisions, busy sessions, quota races, and crash before/after commit. Use existing message and execution tables with transaction locks and versioned canonical request data. Reuse queued recovery; no jobs table or new worker. Test mixed old/new callers. Verify rejected requests cannot steer current work.

### Task 4: Results and interruption

Files: runtime/store plus tests. Add execution-specific bounded queries and output paging. Cover oversized Unicode, no retained final text, failed tasks, cursor tampering and foreign IDs. Test queued cancellation and running interrupt including turnover to a successor. Report interruption delivery separately from verified terminal completion.

### Task 5: Shared operations

Files: new api-rs `session_tools.rs` and `session_operations.rs`, routes/types/client/tests. Construct owner context only from trusted auth. Both adapters reuse these operations, including personal outbound MCP snapshots. No CLI subprocess inside the server. Keep legacy routes compatible and require explicit shared mode when crossing from API-client access to a personally owned session.

### Task 6: Adapters

Files: CLI `mcp_server.rs`, `terminal.rs`, `main.rs`; api-rs `mcp.rs`. Test an actual stdio subprocess with a loopback API fixture, not only a dispatcher mock. Missing credentials fail noninteractively. Share schemas and error/results. No unbounded transcript parsing, hidden credential copies, localhost listener or local durable registry. Match server version; do not fall back to unsafe two-step submission.

### Task 7: Acceptance tests

Add common transport fixtures exercising real router and CLI. Include two users, two connections for one user, API-client personal binding, disabled/revoked access, request collision, reader disconnect, runtime restart, output bounds and cancellation races. Prove database-backed tests ran; fail preflight if required DB variables are absent.

### Task 8: Wiring and handoff

Add disabled-by-default feature/config limits through args/chart schema/template. Keep existing catalog/outbound MCP independent. Update CLI README, plugin docs, API docs and a deployment-neutral session MCP guide. Wire CLI fixtures into Rust CI; `validate-cli-packaging.yml` covers Python tools, not this binary. Set `SESSION_RUNTIME_TEST_DATABASE_URL` explicitly for API tests instead of allowing a skipped pass. Never expose endpoints publicly as part of source implementation.

## Verification

Run focused failing/passing tests at each boundary. Existing CLI fixture invocation:

```bash
CENTAUR_CLI_TEST_BINARY="$PWD/target/debug/centaur-session-cli" \
  python3 crates/centaur-session-cli/tests/cli_configuration.py
```

Build/test Rust with `cargo test --locked` and explicit disposable database variables; run scoped clippy and fmt. Run Rails OAuth/model tests with a separate disposable schema and all configured encryption keys synthetic. Render/lint feature-on/off Helm fixtures under `/tmp`. Final `git diff --check`, source review and compatibility check.

Local acceptance is not production acceptance. Real Pi and ChatGPT OAuth/model/sandbox tests require the relevant live access/authorization and must be reported separately. Kubernetes is read-only under standing policy. No automatic push, merge, deploy or spend permission is conveyed to remote workers.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Identity confusion exposes another user's sessions | Medium | High | Explicit live personal binding; no email inference; negative tests |
| Refresh rotation invalidates healthy connections or enables revoked tokens | Medium | High | Durable connection separate from rotating credential; per-call live check |
| Client UI removes credentials without revoking remotely | Unknown | High | Document explicit server-side disconnect and test actual client behavior |
| Replay or mixed admission races duplicate work | Medium | High | Atomic request validation/persistence and real concurrency tests |
| CLI/hosted protocol differences | Medium | Medium | Shared contract and real transport fixtures |
| Tests silently skip DB paths | High at baseline | High | Mandatory DB preflight in acceptance and CI |

## Open Questions

Shared access and immediate connection revocation are **decided by the user**, not open questions. Remaining deployment questions: chosen endpoint/personas, client-side revoke behavior, resource defaults under load, and real browser/model acceptance. Investigate discoverable details; do not ask for repeated approval of the same implementation.

## ADR Index

| ADR | Title | Status |
|---|---|---|
| Candidate A | Shared durable session operations behind two MCP transports | Proposed implementation shape |
| Candidate B | Personally shared sessions with per-connection immediate revocation | User-approved behavior; concrete mechanism under implementation |

## Progress

Implementation is partial. The original eight-phase draft remains on `docs/session-mcp-plan`; its separate-identity and expiry-only defaults are superseded here.

- Implemented: generated Console migration adds a stable connection UUID to existing refresh-token rows; rotation retains it. Refresh and disconnect use the same transaction-scoped PostgreSQL advisory lock. `POST /mcp/oauth/revoke` accepts a refresh token (including a rotated ancestor) and revokes only that connection. No session cancellation path is invoked.
- Implemented: explicit registered session read/write scopes and human-readable consent; default remains `mcp:tools`. Hosted MCP checks the current connection, account, principal, resource and scopes on every request. Session scopes additionally require the user's personal principal. No email-based linking.
- Compatibility: legacy catalog-only tokens without a connection ID retain their previous lifetime; they cannot acquire session authority. Migrate Console before running the new API/issuer. Old API replicas cannot enforce the new live checks. Client-side credential deletion is not server revocation; the client must call the revocation endpoint. Access-token-only revocation and a Console disconnect UI are not implemented.
- Verified locally: CLI baseline 12 tests; complete API and Console-db suites 269 tests with both database variables set, including an actual hosted dispatcher call accepted before and rejected immediately after revocation using the same unexpired JWT. Rails OAuth/model/user/principal tests pass (98 tests, 814 assertions), as do scoped RuboCop, changed-file rustfmt, CI YAML parsing and `git diff --check`. Strict Clippy is blocked by the unchanged `ConsoleServiceOnly` dead-code warning in `crates/centaur-api-server/src/routes.rs:460`; scoped Clippy passes with only that lint class allowed (`-D warnings -A dead-code`). Synthetic databases only; no real model, Pi/ChatGPT, deployment or rolling-upgrade acceptance.
- Plan validation: the harness-level validator remains rooted in the unrelated Home Manager checkout and reports its six preexisting ADR filename errors. Direct file-level validation of this plan reports only the disclosed PRD-format warning (no separate PRD exists).
- CI: API tests now run separately with an explicit runtime database rather than silently skipping database fixtures in the workspace run.
- Test isolation: Rails fixtures and Rust Console-schema tests require separate disposable databases. The existing Rust schema suite leaves API-client links; subsequent Rails fixture replacement otherwise triggers foreign-key errors before assertions. Do not disable fixture validation.
- Implemented backend: `crates/centaur-session-sqlx/src/delegation.rs` atomically commits a message and queued execution for an existing personal session. Versioned canonical request comparison rejects changed retry payloads; owner-scoped PostgreSQL advisory locks serialize active/rate admission. Retries do not consume quota. The runtime uses its existing dispatch/recovery path after commit and enforces a one-hour maximum/default. Legacy enqueue/append and untargeted interruption refuse the reserved `personal:` namespace to prevent bypass or implicit steering. Creating and binding a new shared session remains application-layer work.
- Implemented results: execution-specific status excludes request/metadata; retained-only event pages are at most 16 KiB, with oversized payloads replaced in SQL above 8 KiB and cursors still advancing. Error text is capped and marked when truncated. Tests cover escaped output, Unicode, foreign execution/cursor rejection, byte limits and combined MCP structured/text size. This is not a promise of complete historical/final output.
- Implemented cancellation: queued cancellation and its terminal event commit together. Running cancellation now uses the distinct `{"type":"interrupt_execution","execution_id":"exe_…"}` command. The shared adapter (including Pi), Codex and Hermes preserve the target through their queues and check it in the active turn consumer before acting. Stale targets emit `centaur/interruptRejected` and cannot cancel a successor. Codex uses its native thread/turn-targeted interrupt. Hermes does not reuse a gateway after a cancellation drain times out: it stops the child rather than exposing a successor to a pending cancellation. The original database-lock-through-stdin approach was insufficient and remains removed.
- Compatibility: updated adapters announce `centaur/executionReady` with `params.execution_id` and integer `params.targeted_interrupt: 1`. The runtime records this only for the matching stdout-owned execution, together with the actual sandbox instance ID. Running delivery requires that execution/instance/version capability; missing identities, missing announcements, unknown versions and unimplemented adapters (including nanocodex) fail closed. There is no untargeted fallback. Old images do not advertise support and their parsers reject the new command. This is local protocol support, not a deployment or mixed-version live acceptance claim.
- Identity authority: `input_line_with_session_context` now overwrites caller-supplied execution IDs with the server-owned ID (preserving other valid metadata). Preserving arbitrary tracing IDs would allow B to masquerade as A at the cancellation boundary; the forged-ID regression failed before this correction.
- Cancellation outcome: targeted `requested` means the command was written to stdin, not acknowledged or completed. `interrupted` is confirmed for atomic queued cancellation or an already-cancelled execution; running delivery returns `requested: true, interrupted: false`. Native terminal output determines durable final status. No terminal cancellation is inferred from delivery, and a delivery timeout reports an unknown outcome.
- Regression fixture: the concurrent-Pi test failed on unchanged baseline `915033a3` as well as the feature tree. Its started event precedes sandbox allocation, while mock I/O is FIFO. The fixture now waits for actual input delivery and verifies its thread key before admitting the second session; ten consecutive focused runs passed. This preserves concurrent active work and disconnected-reader coverage without changing production execution behavior.
- Build isolation: baseline and feature worktrees must use distinct Cargo target directories. Sharing the target during comparison reused an old SQLx artifact and produced missing-method errors despite current source; cleaning the affected workspace packages restored a correct build. Do not paper over this with source/API changes.
- Verified after the fail-closed correction: `cargo test --locked -p centaur-session-sqlx -p centaur-session-runtime -p centaur-api-server -- --test-threads=1` passes all 472 tests with explicit disposable database configuration, including seven new atomic-turn integration tests. The running-interrupt regression failed before the correction and passed afterward. Changed-file rustfmt and `git diff --check` pass. All-targets scoped Clippy passes with `-D warnings -A dead-code`; the previously disclosed strict baseline dead-code limitation remains. No real sandbox/model or client transport acceptance is inferred from these tests.
- Verified targeted interruption: full `cargo test --locked -p harness-server -p centaur-session-runtime -p centaur-session-sqlx -p centaur-api-server -- --test-threads=1` passed 663 tests (five explicitly ignored real-provider tests). After the final Hermes error-recovery hardening, all 191 harness tests were rerun successfully. Real adapter loops with fake Pi/Codex/Hermes children reject delayed predecessor interrupts and cancel matching successors; Pi/Hermes also cover duplicate targets, and Hermes covers replacement of a failed gateway. Runtime fixtures reject absent/unknown-version capabilities and wrong instances, do not reuse A's capability for B, and distinguish request delivery from a later durable cancellation event. Parser tests reject missing/invalid targets without legacy downgrade. Changed-file rustfmt, `git diff --check`, and scoped all-target Clippy pass; strict baseline dead-code limitation remains as disclosed above. No real provider, deployment or client acceptance performed.
- Remaining: shared session owner/REST authorization and initial session preparation, six tool contracts, CLI stdio and hosted dispatch, configuration limits/feature gates, and end-to-end transport/recovery coverage. The feature is not usable yet. The reviewed foundation is being recorded in a local feature-branch commit; no push or deployment is authorized.
