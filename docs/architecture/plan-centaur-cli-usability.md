---
title: Centaur CLI usability consolidation
prd: "Not applicable: direct user review requirements"
date: 2026-11-20
author: Pi
status: Draft
---

# Plan: Centaur CLI Usability Consolidation

> **Execution guidance:** Use the executing-plans skill when implementation is explicitly authorized. The user subsequently authorized implementation. Commit, publication and deployment remain unauthorized.

**Goal:** Make the existing remote-session CLI predictable and usable without adding a second session, identity, or orchestration system.

**Architecture:** Keep the existing Rust executable and authenticated HTTP client. Human CLI and MCP transports consume the same server-owned operations once those operations exist; legacy sessions retain their existing access boundary. Fix independent interface defects first and gate shared-session integration on the unfinished MCP work.

**Tech Stack:** Rust, Clap, Reqwest, Ratatui/Crossterm; existing Python loopback CLI fixtures and Rust API/runtime tests.

## Source

* **Requirements:** User-requested CLI usability re-review and follow-up request to create a plan; no separate PRD.
* **Date:** 2026-11-20.
* **Author:** Pi.
* **Verified baseline:** `721edc326228c260a664dd11f5fffabaea050aec`. CLI files are unchanged from `915033a3`.
* **Related plan:** [Session MCP implementation](plan-centaur-session-mcp.md), especially its Progress section. Atomic delegation and connection checks exist; shared owner/REST authorization, tool contracts, and CLI/hosted adapters remain unfinished. A foundation commit is not a completed MCP product.
* **Current client contract:** [CLI README](../../crates/centaur-session-cli/README.md).
* **Observed checks:** Installed CLI help inspected; all 12 existing black-box configuration tests passed against synthetic local servers. A loopback capture showed top-level `--harness-type pi` followed by `chat new` emits a Codex creation request. Its mock response did not complete the session, so this proves request selection only.

### Requirements and acceptance criteria

| ID | Requirement                              | Acceptance criterion                                                                                                                                                              |
| -- | ---------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R1 | No silently ignored options              | Explicit legacy execution flags combined with modern subcommands fail before any network request, with the correct modern spelling where applicable.                              |
| R2 | Honest onboarding                        | Authentication success is distinguished from session readiness; missing principal/permissions produce actionable, secret-free guidance. No automatic identity creation or grants. |
| R3 | Reversible local configuration           | Redacted inspection, key removal, and local credential removal work without exposing secrets or implying server revocation.                                                       |
| R4 | Truthful command semantics               | Help states fresh/reuse, enqueue/completion, transcript/events, and integration/serving distinctions. Compatibility aliases are explicit.                                         |
| R5 | Execution-oriented automation            | Successful submission exposes the returned execution ID; machine output is explicit and diagnostics stay on stderr.                                                               |
| R6 | Consistent interactive behavior          | Reattachment has an explicit per-turn duration policy; basic Unicode-safe cursor editing works without changing detach/interrupt semantics.                                       |
| R7 | Shared operations, not duplicated policy | Shared-mode CLI operations use the same authorized server operations as MCP; no client namespace inference, silent migration, or two-request fallback.                            |
| R8 | Preserve safety and compatibility        | Existing machine sessions, scoped credentials, destructive confirmations, pagination, replay, and retry protections continue working.                                             |

## Architecture Overview

```text
Human terminal CLI ─────────────────────┐
Local agent → CLI MCP stdio adapter ────┼── authenticated server operations
Hosted client → existing HTTP MCP ──────┘              │
                                                       ▼
                                       existing runtime + PostgreSQL
```

The diagram describes the target, not completed baseline behavior. The session MCP plan owns shared authorization, atomic submission, execution results, targeted interruption, feature gates and both MCP adapters. This plan owns the human-facing command contract and adapting it to those operations after the dependency gate passes.

Retain a legacy compatibility path for existing API-client sessions. Do not turn `api:<client-id>:` keys into personal keys on the client or permit raw keys to bypass authorization. Shared access must be explicit and server-authorized. A linked personal principal is not equivalent to an arbitrary principal whose display name matches a user.

**Options considered:** (1) leave both CLI surfaces as-is: least code, but preserves verified silent misconfiguration; (2) consolidate the existing CLI: recommended, reuses upstream capabilities and limits migration; (3) rewrite the CLI or replace it with MCP: rejected, discards existing human workflows without fixing server contracts.

### Scope and non-goals

* Keep core chat operations and guarded lifecycle commands. Do not delete commands merely to reduce their count.
* No new runtime, local database/session registry, queue, auth store, SDK framework, or administration surface.
* No automatic principal creation, default-role assignment, secret grants, OAuth-account copying, or server-side credential revocation from local logout.
* No full transcript reconstruction until the server has a sufficient ordered contract; raw retained events are not a complete conversation.
* No named profiles initially: document existing `CENTAUR_CONFIG` isolation for separate installations.
* No new model-selection/resource provisioning flags: harness/persona and deployment controls already exist.
* Implementation was subsequently authorized by the user. No commits, publication, deployment, or paid model workloads are authorized.

## Components

### Command surface and compatibility

**Files:** `crates/centaur-session-cli/src/main.rs`, `src/terminal.rs`, `README.md`, `tests/cli_configuration.py` (all relative to that crate after the first path).

Use modern subcommands as the primary help surface; preserve legacy-only invocations. Distinguish explicitly supplied root flags from defaults, so merely having Clap default values does not reject every subcommand. Connection flags such as `--api-url` remain valid where already supported. Deprecation notices go to stderr, never protocol stdout.

**ADR reference:** Candidate A below; compatibility boundary is proposed, not an approved breaking release.

### Configuration and diagnostics

Reuse the existing protected configuration and credential-to-endpoint binding. Proposed local commands: `config show`, `config unset <key>`, and `logout`. Show resolved non-secret settings, their sources, config path, and credential presence only; never serialize the whole config object. Local logout removes the saved secret and binding metadata, retaining endpoint/client selection. Environment credentials are not removed; explain that explicitly.

Authentication is not readiness. A non-mutating readiness check requires server evidence for attached principals and scopes; the baseline listing API alone cannot prove this. Until the shared API supplies that evidence, label readiness unknown and translate the known missing-default error into a precise Console remediation. Never create a throwaway remote session as a diagnostic.

**ADR reference:** None; reuse current storage/security rules.

### Session results and interactive use

Retain current human defaults while adding explicit `--json` for selected command outputs, beginning with submission receipts, list and status. `logs` remains JSONL. Reuse the server response rather than inventing a local execution registry. A successful enqueue is not completion; errors remain nonzero with stderr diagnostics.

Add explicit per-turn duration to `attach`, retaining the existing one-hour default. It affects newly submitted turns from that attachment, not an already-running execution or persistent session policy. Keep milliseconds for compatibility; human-duration parsing is out of scope.

**ADR reference:** Shared identity/submission decisions remain owned by the related MCP plan.

## Implementation Order

| Phase | Component                                               | Dependencies                                         | Estimated Scope |
| ----- | ------------------------------------------------------- | ---------------------------------------------------- | --------------- |
| 1     | Rebase/review baseline and command matrix               | None                                                 | S               |
| 2     | Reject conflicting flags and improve help               | Phase 1                                              | S               |
| 3     | Reversible config and honest diagnostics                | Phase 2                                              | M               |
| 4     | Command semantics and integration naming                | Phase 2                                              | S               |
| 5     | Submission receipts, output and interactive consistency | Phases 2–4                                           | M               |
| 6     | Shared-session integration gate                         | Related MCP plan shared operations and authorization | M               |
| 7     | Wire human commands to shared operations                | Phases 5–6                                           | M               |
| 8     | Compatibility, documentation and acceptance             | Phases 1–7                                           | M               |

Phases 1–5 can proceed before MCP adapters finish, but coordinate changes to `main.rs` and `terminal.rs` with that work. Phases 6–7 must not invent missing endpoints or repeat the backend implementation. No delegation is implied by this sequencing.

### Task 1: Establish the executable and compatibility baseline

**Files:** `src/main.rs`, `src/terminal.rs`, `src/tui.rs`, `tests/cli_configuration.py`, related MCP plan.

1. Inspect clean/dirty state and nearest instructions in an implementation worktree; read current dependent changes rather than assuming this baseline is still current.
2. Compare installed binary help with a freshly built worktree binary. Record which binary each test exercises.
3. Enumerate root legacy flags, modern commands, their defaults, environment sources, HTTP operations, output and side effects.
4. Run the existing fixture suite. Reproduce conflicts with synthetic loopback requests; never use production credentials.
5. Confirm no concurrent implementation already addresses a finding. Update only this plan's progress/evidence if needed.

**Done:** Every proposed change maps to a current behavior and a runnable regression; failures in unrelated baseline checks are recorded separately.

### Task 2: Reject ignored legacy flags and clarify help

**Files:** `src/main.rs` parser/tests; `src/terminal.rs` help; `README.md`; `tests/cli_configuration.py`.

1. Add failing cases for explicit `--harness-type`, `--message`, `--thread-key`, `--attach`, `--input-line`, `--idle-timeout-ms`, `--max-duration-ms`, event/exit flags, `--stdin-events`, `--tui`, and `--debug` before modern subcommands. Distinguish legacy execution flags from valid connection settings.
2. Add passing cases for legacy-only invocations, clean modern invocations, modern duration/harness options and supported endpoint overrides.
3. Use Clap conflict/value-source support to reject explicit incompatible flags before config loading or network traffic; do not infer presence from a default value.
4. Add concise descriptions and examples for common commands, credential source precedence, enqueue behavior and detach versus interrupt. Put legacy usage in a clearly labeled section without removing it in this patch.
5. Run parser tests and black-box tests; assert invalid combinations make zero fixture HTTP requests.

**Done:** The previously observed Pi-to-Codex mismatch is impossible without an explicit error; normal legacy behavior remains covered.

### Task 3: Make setup inspectable and reversible

**Files:** `src/terminal.rs`; `tests/cli_configuration.py`; `README.md`. Server/client changes only after Task 6 identifies an existing readiness contract.

1. Write failing fixture tests for redacted `config show`, clearing principal override, logout with absent/present credentials, environment overrides, and mode-0600 atomic persistence.
2. Implement `config show`, `config unset` for the existing allowed keys, and local `logout`. Removing endpoint/client must invalidate saved credentials consistently with changing them. Removing principal must restore default selection for new sessions.
3. Keep secret values out of stdout/stderr, including malformed configuration and errors. Report source and presence, not token/secret contents. State that logout neither revokes remote credentials nor stops work.
4. Clarify login output: token authentication verified; session readiness not established unless verified by a supported read-only endpoint. Translate the no-default-principal error without hardcoding an identity or copying infra permissions.
5. Document an operator handoff: supply endpoint/client credentials and an approved attached principal. Personal MCP additionally needs verified account linkage. Do not invent a provisioning UI path.
6. Test insecure files, endpoint mismatch, denied auth, and environment credentials after logout. Rerun the existing security tests unchanged.

**Done:** Users can inspect and undo setup without opening the secret-bearing file; unknown readiness is never reported as success.

### Task 4: Correct names without a surprise breaking release

**Files:** `src/terminal.rs`; `tests/cli_configuration.py`; `README.md`.

1. Add tests for help/output distinguishing create-or-reuse, enqueue-only, retained events, and external integrations.
2. Immediately document `chat new` as create-or-reuse and report whether a session was reused only when the response proves it. Do not add a racy existence probe or fabricate freshness. True fresh-only behavior waits for Task 7.
3. Proposed primary integration commands: `integrations list` and `integrations connect <name>`, delegating to the existing list/login implementation. Preserve `mcp list/login` as documented compatibility aliases.
4. Retain `mcp add` temporarily with an explicit deprecation explanation: it validates an already registered URL and starts consent; it does not register a server. Do not add a second integration store.
5. Reserve `mcp serve` for the adapter owned by the related plan; do not implement it here. Explain inbound serving versus outbound integrations in help.
6. Describe `history` as persisted messages rather than a complete transcript; preserve its current output until a versioned output change is tested.

**Done:** Existing invocations continue to work, and each command accurately describes its side effects. Alias removal needs a separately communicated compatibility decision.

### Task 5: Expose receipts and improve ordinary use

**Files:** `src/terminal.rs`, `src/tui.rs`; existing `src/main.rs` callers of send helpers; `tests/cli_configuration.py`; `README.md`.

1. Write fixture tests for submission receipts using the actual execute response shape: execution ID, thread key, status and request ID. Test failure after append but before enqueue without implying a successful receipt.
2. Return the execute response from the send helper rather than discard it; update every TUI/CLI caller. Print an explicit queued/accepted receipt, not a completed result. Retain the pre-submission request ID on stderr for ambiguous failures.
3. Add `--json` to submit/send, list and status only where a stable tested contract is defined; no ANSI/prose on JSON stdout. Preserve existing defaults and JSONL logs. Do not describe legacy append+enqueue as atomic.
4. Add `chat attach --max-duration-ms` with the current default. Test that it controls subsequent sends only; do not change an already-running execution.
5. Add Unicode-safe cursor navigation, insertion and deletion to the existing TUI editor using existing dependencies. Cover multibyte input, paste, history recall and cursor reset. Preserve Ctrl-C interrupt and Ctrl-D detach. Do not build a full editor framework.
6. Run parser, TUI and loopback tests. Verify Enter submits exactly the displayed edited text and JSON output parses as one documented value.

**Done:** A script can identify the execution it submitted; reattached human sessions have explicit duration behavior and basic text editing works.

### Task 6: Verify the shared-session dependency gate

**Files to inspect:** related MCP plan; `crates/centaur-api-server/src/{routes,types,client,mcp}.rs`; `crates/centaur-session-runtime/src/lib.rs`; `crates/centaur-session-sqlx/src/delegation.rs`; actual shared-operation modules introduced by that work.

Do not assume the proposed `session_operations.rs` or `session_tools.rs` exists: verify the implemented layout first.

1. Verify shared owner resolution, initial session creation, REST access, client methods and capability/feature checks are implemented and tested.
2. Confirm atomic admission, retry-payload mismatch handling, execution-specific results, targeted cancellation and revocation behavior are available through the public boundary, not merely private runtime methods.
3. Verify explicit legacy-versus-shared selection and cross-client discovery through two identities; assert an unrelated user is denied.
4. Resolve how the CLI obtains read-only readiness/identity information and distinguishes an unsupported server from missing permission. Prefer an existing response; do not add a second authorization service.
5. Record the tested server/client compatibility and actual endpoints/types in this plan before starting Task 7.

**Done:** A bounded public request can submit/read/cancel its own shared execution with negative authorization coverage. Otherwise stop the integration phase and retain the independent usability fixes.

### Task 7: Adapt the human CLI to the shared contract

**Files:** `src/terminal.rs`, `src/tui.rs`, API client/types, existing CLI/API integration fixtures; exact shared modules confirmed in Task 6.

1. Write failing tests for one personal session visible through two authorized connections, machine-session isolation, disabled/revoked identity, unsupported server, fresh-name collision, and ambiguous submission retry.
2. Reuse the shared server client methods. Resolve owner and shared session references on the server; never derive personal ownership from an email, arbitrary principal string or user-supplied thread prefix.
3. Expose explicit shared selection consistent with the MCP plan. Choose its exact CLI spelling against the implemented contract, not before; preserve the legacy default until a separately approved migration.
4. In shared mode, submit atomically and consume execution receipts/results. Never silently fall back to legacy two-step submission or untargeted interruption. Keep legacy commands clearly labeled where semantics differ.
5. Add fresh-only creation only if the server can atomically reject a name collision; make reuse intentional. Preserve existing legacy create-or-reuse compatibility with clear help/deprecation.
6. Use execution IDs for targeted interruption and results. Detach, client EOF and reader failure must not cancel accepted work. Report requested interruption separately from confirmed termination.
7. Verify readiness using the contract from Task 6 without creating sessions or changing permissions.

**Done:** CLI and MCP are alternate clients of the same shared behavior, not separate policy implementations. Existing machine sessions remain accessible only within their original boundary.

### Task 8: Final validation and handoff

**Files:** CLI README/tests; related API transport tests; `.github/workflows/ci.yml` only if a missing check is verified.

1. Run all narrow checks below; verify mandatory DB fixtures executed for shared changes rather than silently skipping.
2. Exercise first-time setup, wrong principal, logout/environment override, repeated name, send receipt, attach/detach, retry, raw logs and guarded cleanup with disposable fixtures.
3. Run the same shared-session scenarios through CLI and hosted/stdio MCP once those adapters exist. Scope mismatch, revoked connection and wrong-owner requests must be denied consistently.
4. Update documentation for supported server/client combinations, retained-history limits, JSON receipts and legacy compatibility. Avoid stale statements that every advertised harness is deployed/credentialed.
5. Record known limitations and exact tested revisions. Live deployment/model acceptance requires separate authorization and remains a release gate, not something to simulate and call complete.

**Done:** User-facing acceptance is demonstrated, not inferred from parser tests. Any authorized implementation commits should be small conventional commits per completed boundary; this planning request does not authorize commits or publication.

## Verification

Run project commands from the implementation worktree with its environment loaded. Use an isolated target directory under `/tmp` to avoid cross-worktree stale artifacts:

```bash
cd "$WT"
TARGET_DIR="$(mktemp -d /tmp/centaur-cli-check.XXXXXX)"
direnv exec "$WT" env CARGO_TARGET_DIR="$TARGET_DIR" cargo test --locked -p centaur-session-cli
direnv exec "$WT" env CARGO_TARGET_DIR="$TARGET_DIR" cargo build --locked -p centaur-session-cli
direnv exec "$WT" env CENTAUR_CLI_TEST_BINARY="$TARGET_DIR/debug/centaur-session-cli" \
  python3 crates/centaur-session-cli/tests/cli_configuration.py
direnv exec "$WT" cargo fmt -p centaur-session-cli --check
direnv exec "$WT" env CARGO_TARGET_DIR="$TARGET_DIR" \
  cargo clippy --locked -p centaur-session-cli --all-targets --no-deps -- -D warnings
git diff --check
```

Expected: new regressions fail before their fix and pass afterward; all baseline tests remain green. If a preexisting lint/toolchain failure occurs, reproduce it on the unchanged base rather than suppressing unrelated checks.

For Tasks 6–8, follow the related MCP plan's disposable DB preflight and real-router tests. Set both `SESSION_SQLX_TEST_DATABASE_URL` and `SESSION_RUNTIME_TEST_DATABASE_URL` through protected local configuration, never real credentials or hardcoded guessed DSNs. Rails and Rust schema fixtures require separate disposable databases. No Kubernetes mutation is authorized under standing policy.

## Risks and Mitigations

| Risk                                                       | Likelihood | Impact | Mitigation                                                                                     |
| ---------------------------------------------------------- | ---------- | ------ | ---------------------------------------------------------------------------------------------- |
| Overlap with unfinished MCP implementation                 | High       | High   | Coordinate file ownership; block shared integration on public contracts and tested authority.  |
| Breaking existing scripts while improving names/output     | Medium     | High   | Preserve legacy paths and output defaults; opt-in JSON and compatibility aliases.              |
| Config display/logout exposes or misrepresents credentials | Medium     | High   | Allowlist display fields; test all sources; local removal is not remote revocation.            |
| Fresh-only creation races with another caller              | Medium     | High   | Server-enforced semantics; no check-then-create workaround.                                    |
| Readiness check has side effects or overclaims access      | Medium     | High   | Read-only contract; report unknown when unavailable; never create test sessions automatically. |
| Shared access accidentally broadens machine namespaces     | Medium     | High   | Explicit shared mode; trusted owner resolution and wrong-user negative tests.                  |
| UI cleanup expands into another agent runtime              | Medium     | Medium | Bound scope to editing/rendering; model/tools/workspaces remain server-owned.                  |

## Open Questions

* What exact read-only readiness and shared-session reference contract will the MCP work expose? Task 6 resolves this from implementation evidence.
* What exact syntax will select shared sessions without changing legacy defaults? Decide once against the completed contract, then update Task 7 and help tests.
* When may deprecated aliases or legacy create-or-reuse semantics be removed? No removal is authorized by this draft; retain compatibility until a release decision.

These do not block Tasks 1–5. Neither a new permission model nor a replacement CLI is an open design requirement.

## ADR Index

| ADR         | Title                                                                               | Status                                                                                                         |
| ----------- | ----------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Candidate A | Preserve legacy machine sessions while adding explicit personally shared CLI access | Proposed; align with shared identity ADR candidates in the MCP plan rather than creating a competing decision. |

No ADR files are created by this plan request. Naming/help/config changes do not need standalone ADRs.

## Progress

* Draft created from verified baseline and the repeated usability review.
* Independent CLI work (Tasks 1–5) is implemented and locally verified in `fix/cli-usability`. No new dependencies or server/API changes.
* Task 6 was rechecked at baseline 721edc32: public shared owner/REST operations and adapters remain absent. Task 7 and cross-transport acceptance in Task 8 remain blocked; no substitute protocol or namespace bypass was added.
* Task 8 is complete only for the independent CLI surface: README updated; 13 Rust tests and 23 Python black-box tests passed against the freshly built worktree executable. Build, format check, CLI-scoped strict Clippy (`--no-deps -- -D warnings`) and whitespace checks passed.
* All 12 original Python fixtures remain passing. Added cases cover every explicit legacy flag conflict with zero network traffic, legacy-only Pi execution and duration, config redaction/unset/logout, readiness diagnostics, retained integration aliases, receipt JSON/failure, paginated JSON/failure, and validation before session creation.
* A real PTY test drives the built TUI against a loopback HTTP fixture: edits `a界🙂z` into `a界é`, verifies the submitted payload and attach timeout `1234`, observes the execution receipt, and detaches without submitting an interrupt. Rust renderer checks cover scalar-safe editing, history, cursor placement and tiny terminal sizes. This is synthetic client-path proof, not deployed agent/model acceptance.
* Test-first failures reproduced the ignored-option/config/output gaps and missing editor behavior before their fixes. Follow-up negative tests caught blank `new --message` creating a session and a zero duration being accepted; both now fail before requests. The initial PTY fixture lacked a cursor-position reply; it was corrected to implement Crossterm's DSR handshake, not by weakening the application.
* Validation used the worktree binary under `/tmp/centaur-cli-usability-target`, not the installed CLI. Rustfmt and Clippy were supplied through the local Nix toolchain. Unscoped strict Clippy fails on the preexisting unused `ConsoleServiceOnly` variant in `centaur-api-server/src/routes.rs:460`; the CLI-scoped run passes without suppressing CLI warnings. The dependency still emits its existing warning during build/test.
* No live deployment, real-model workload, personal-principal provisioning, permission/configuration changes outside fixtures, commit or publication. Changes remain in the implementation worktree; the earlier planning worktree is untouched.
* Plan remains `Draft` because shared integration and cross-transport acceptance are blocked. Structural validation retains the disclosed PRD-format warning: requirements came directly from the user rather than a formal PRD.
