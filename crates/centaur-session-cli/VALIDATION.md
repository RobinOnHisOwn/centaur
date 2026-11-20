# Validation record

Verified locally on 2026-11-12. Base revision: `6e293ead961dcd7800a1c6dba456d6f8e5b1e9ee`; branch: `feat/terminal-workflow`. No deployment or cluster mutation was performed. Git history records subsequent source publication; publication does not establish live acceptance.

## Console administration — 2026-11-20

First-delivery implementation on `feat/cli-administration`, based on `e94d1cfb4a469abc60b89b959407c7dfe00f6f63`. The following is local Linux evidence, separate from the historical macOS/session validation below. No real credentials, deployment, installed executable, dependencies or lockfiles were changed; source/build artifacts only. Console-only users/API clients/principal deletion remain gated by the [plan](../../docs/architecture/plan-centaur-cli-administration.md).

| Check | Observed result | Scope |
| --- | --- | --- |
| CLI Rust unit tests | 13 passed | Existing parser/editor/session checks, Rust 1.99.1 |
| Existing executable configuration fixture | 23 passed | Chat login, configuration, events and lifecycle remain compatible |
| New `tests/cli_admin.py` | 15 passed | Loopback HTTP and subprocess commands, synthetic credentials only |
| Ten affected Rails request suites | 250 runs, 4,875 assertions; no failures/errors/skips | Actual disposable PostgreSQL/pgvector 16; includes credential namespace sweep and existing Console-only/sandbox tests |
| Real CLI → Rails → PostgreSQL | Passed | Separate disposable database, loopback Rails server; role/principal upsert/get, exact dotted identity round-trip, assignments, label pagination, redacted config, grant create/repeat/read/list/delete, update-only failure and role deletion |
| CLI strict Clippy | Passed with `--no-deps --all-targets -- -D warnings` | Existing dependency dead-code warning for `RouteAccess::ConsoleServiceOnly` remains; CLI has no new warnings |
| Changed Ruby files RuboCop | Passed | Controllers and their regression tests |
| Full Console RuboCop | Existing unrelated failure | Extra blank line in unchanged `personal_mcp_connections_controller.rb:19`; reproduced from baseline source, not corrected here |
| Canonical `bin/brakeman` | Blocked by its latest-version check | Locked Brakeman 8.1.0 reports 8.1.1 available; no dependency update made |
| Supplemental pinned `bundle exec brakeman` scan | 0 scanner errors, 0 security warnings | Scanner 8.1.0 only; this does not satisfy the canonical latest-version gate. Existing Bundler constant-redefinition warnings also appear. |

The new executable fixture was first run against the baseline and failed because `admin` did not exist. Added server tests first reproduced ambiguous grants being created and concurrent role upserts failing at both validation/database uniqueness guards; the minimal shared-controller fixes pass those tests. A separate stdin test first demonstrated silent truncation of an oversized credential, then verified rejection without saving or networking.

The real HTTP probe caught a limitation in the original synthetic fixture: Rails interpreted literal dots as format suffixes, and matching returned OIDs alone could falsely pass after writing the wrong foreign ID. A direct percent-encoded request verified the established routing behavior; the shared client request boundary now encodes dots. The permanent executable fixture checks encoded requests, and the real HTTP check asserts the returned `foreign_id` exactly equals the requested value before checking OIDs.

Reproduce the CLI checks from the repository root with its Rust toolchain loaded and `CENTAUR_CLI_TEST_BINARY` set to the actual built binary:

```bash
cargo test --locked -p centaur-session-cli
cargo build --locked -p centaur-session-cli
python3 crates/centaur-session-cli/tests/cli_configuration.py
python3 crates/centaur-session-cli/tests/cli_admin.py
cargo clippy --locked -p centaur-session-cli --all-targets --no-deps -- -D warnings
cargo fmt --all -- --check
```

For the ten Rails suites, use the [plan's verification commands](../../docs/architecture/plan-centaur-cli-administration.md#verification) including `test/controllers/api/v1/sandbox_admin_controller_test.rb`, with `PARALLEL_WORKERS=1` and dedicated `CENTAUR_CONSOLE_TEST_DB_NAME`/`CENTAUR_CONSOLE_DEV_DB_NAME` values. Do not reuse Rust Console-schema fixture databases or any deployment database. The real HTTP check used another dedicated database and synthetic active admin/operator key plus a placeholder secret record, never a real provider credential or sandbox. It starts no chat execution and proves no deployment ingress, SSO, real proxy traffic or model behavior. Test servers were stopped afterward. Build/install/deployment remain distinct.

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
