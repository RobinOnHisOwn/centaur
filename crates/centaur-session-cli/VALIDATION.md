# Validation record

Verified locally on 2026-11-12. Base revision: `6e293ead961dcd7800a1c6dba456d6f8e5b1e9ee`; branch: `feat/terminal-workflow`. No deployment or cluster mutation was performed. Git history records subsequent source publication; publication does not establish live acceptance.

## Delivered artifacts

- Existing `centaur-session-cli` extended, not replaced with another runtime/client platform.
- api-rs adds namespace-scoped discovery and authorized state reads. No schema migration, controller, Pod operation, session registry or filesystem-sharing mechanism added.
- Workspace root `centaur` is an optimized **macOS arm64** executable (about 7.7 MiB), with an ad-hoc signature. `--help`, command help and black-box OAuth/configuration tests ran against that executable.
- `otool -L` confirmed only macOS system libraries/frameworks after changing the Nix build's unused libiconv load reference to `/usr/lib/libiconv.2.dylib`. System library loading and `codesign --verify --strict` passed. This is not a notarized or cross-platform distribution; other OS/architectures should build from source.
- `source/` points to the isolated worktree; `upstream/` is its durable common repository. README and assessment links at the workspace root point into the worktree. Source changes remain separate from deployment; review the feature branch before integration.

## Local evidence

| Check | Observed result | Scope |
| --- | --- | --- |
| Locked optimized CLI build | Passed | Rust 1.91.1 / Cargo 1.91.0 on macOS arm64 |
| Four affected packages, including integration/doc tests | 456 reported passed, zero failed | Actual PostgreSQL 17.10 + pgvector 0.8.2; runtime sandbox backends are test doubles |
| Built-executable configuration/auth tests | 4 passed | Real subprocess and loopback HTTP, fixture credentials only |
| `cargo fmt --all --check` | Passed | Repository formatting |
| `git diff --check` | Passed | Edited tracked files |
| Strict Clippy | Blocked by existing `nonminimal_bool` at `centaur-api-server/src/slack_proxy.rs:1088` | That file is unchanged |
| Four-package Clippy with only `clippy::nonminimal_bool` allowed | Passed | All targets; remaining warnings denied |
| Existing Pi stdio fixture test | Not run successfully | Upstream Nanocodex dependencies require Rust 1.97; local toolchain is 1.91.1 |
| Live Pi/Kubernetes acceptance | Not run | No configured Centaur endpoint/API client/provider credentials; no cluster mutation authorization |

The Cargo count includes upstream migration tests whose ParadeDB/legacy-BM25-specific branches return early when `pg_search` is unavailable. PostgreSQL/pgvector migration, namespace discovery, public Pi creation/state, execution and recovery checks actually used the disposable database. Do not interpret the count as full ParadeDB coverage or real Kubernetes/model execution.

### Added checks

- Command parsing for configuration/login/new/attach/list/status/interrupt.
- Human-name namespace validation and HTTPS/loopback URL restrictions.
- Actual public API Pi session creation and authorized durable message/state retrieval.
- Literal-prefix SQL discovery (including an underscore near-collision), pagination, execution status and event high-water marks.
- API-client namespace boundaries for collection and individual state reads.
- Two concurrent Pi-harness sessions with different mock sandbox assignments; both remain running after event readers are dropped, complete independently and replay only their own output.
- HTTP reconnect from the last event ID, duplicate suppression and token reacquisition after rejection; forbidden responses are not treated as transient failures.
- Pi normalized assistant/tool output, multiline paste, prompt history and client-only detachment.
- Actual CLI login exchange, 0600 credential file, credential-to-endpoint binding, secret redaction, rejected-login non-persistence and environment-secret operation without an interactive prompt.

Upstream tests also cover queued durable dispatch, orphan adoption, stdout ownership handoff, reattachment to recorded output, failed/vanished sandboxes, sandbox replacement and harness-thread preservation. Those are local backend/runtime tests, not evidence that an actual tool resumes after a Pod restart.

## Reproduce

Use a disposable PostgreSQL database with pgvector and export its connection URL as `SESSION_RUNTIME_TEST_DATABASE_URL`. Do not point the tests at production: upstream recovery tests clear fixture state. Run the four affected packages with `--test-threads=1`:

- `centaur-api-server`
- `centaur-session-cli`
- `centaur-session-runtime`
- `centaur-session-sqlx`

The validation run used `cargo test` with all four `-p` selections in one invocation. No global package installation was needed; missing Rust/Postgres tools were supplied by temporary Nix shells. PostgreSQL listened only on loopback in a disposable `/tmp` data directory.

The built-binary tests use Python's standard library. Set `CENTAUR_CLI_TEST_BINARY` to the absolute executable path, then run:

```bash
python3 crates/centaur-session-cli/tests/cli_configuration.py -v
```

For a deliberately authorized deployment test, configure the CLI and run:

```bash
python3 crates/centaur-session-cli/tests/live_workflow.py --cli centaur
```

The live test spends tokens, starts two `accept-*` sessions, kills local event readers, checks remote completion and actual bash output, reconnects by cursor and continues each session. Both sessions use the same scratch pathname with different values to test writable isolation. They remain available for inspection; release them explicitly when finished.

## Reliability acceptance boundary

| Scenario | Local evidence | Still needed |
| --- | --- | --- |
| Terminal closes / CLI reader crashes | Dropping readers does not cancel concurrently running runtime tasks | Kill a real terminal during a real Pi task |
| Laptop loses connectivity | HTTP reader reconnect/cursor tests; server task ownership verified | Real connectivity outage against a configured deployment |
| Reconnect and inspect results | Persisted API state and independent event replay tested | Real native Pi conversation continuation |
| Multiple agents execute | Concurrent runtime sessions, distinct assignments and scoped output tested | Actual sandbox writable isolation (opt-in script provided) |
| API temporarily disconnects | SSE error/EOF/reconnect and rejected-token renewal tested | Deployment ingress/network validation |
| Agent execution fails | Existing failure persistence/recovery tests, UI failure rendering, state error field | Provider/tool failure on actual deployment |
| Control-plane restart | Upstream adoption and stdout ownership tests passed | Rolling-restart acceptance with live tasks |
| Sandbox Pod restarts | Existing replacement/recovery paths tested with backend doubles | Operator-led Pod replacement with state PVC enabled |

Native Pi history/filesystem recovery needs the existing state volume, which defaults off. Final sandbox release deletes that state. An interrupted/lost tool is not automatically rerun or resumed. Older stdout events may be removed by retention. Normalized Pi text is emitted at message completion, not token-by-token. Per-create resource/repository provisioning remains unsupported. Added session lifecycle checks cover default archive filtering and inclusive listing, unarchive, busy/workflow refusal, failed cleanup rollback, cascading history deletion, concurrency with execution admission, event-stream closure, namespace restrictions and CLI confirmation/HTTP contracts. Prune tests cover 100-item pagination after eligibility filtering, archived inclusion, namespace/active/workflow exclusions, each recent-activity source, age rechecking before sandbox cleanup, dry-run confirmation, old-backend refusal, partial cleanup errors and interrupted scans. Local backend doubles and disposable Postgres are not live deletion acceptance; deploy the matching API before exercising these commands remotely.

## Reusable procedure / memory

The verified validation procedure is recorded here. The available `skill_manage` interface has no source-path argument, while this setup forbids writing skills into deployed `~/.pi` paths. No managed runtime skill was changed or activated as a workaround. A future source-managed skill can reference this record rather than copying implementation details.

The topical Notion memory contains a reference to these source documents and the unresolved live-acceptance blockers, not copied code or credentials.
