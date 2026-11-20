---
title: Centaur CLI administration
prd: "Not applicable: direct user planning request"
date: 2026-11-20
author: Pi
status: Draft
---

# Plan: Centaur CLI Administration Implementation Plan

> **REQUIRED SUB-SKILL:** Use the executing-plans skill to implement this plan task-by-task. Implementation was authorized on 2026-11-20; phases 1–5 are locally implemented and verified. Phases 6–9 remain blocked by their explicit authentication decision. No commit, publication or deployment was requested.

**Goal:** Add explicit administration subcommands to the existing `centaur` executable, starting with supported operator APIs and preserving the option of a future Terraform/OpenTofu provider.

**Architecture:** Keep the CLI thin: Clap dispatches `centaur admin ...` to Console's existing HTTP operations, while Console remains responsible for authorization, validation and persistence. Keep operator credentials separate from chat-client credentials. Deliver bearer-supported resources first; user management, API-client management and principal deletion remain gated on a separately approved authentication design.

**Tech Stack:** Existing Rust/Clap/Reqwest/Serde JSON, Rails Console APIs, Rust unit tests, Python standard-library loopback fixtures and existing Rails request tests. No new dependency is assumed.

## Source

* **Requirements:** User requested a plan for individual administration subcommands in the existing CLI. A provider is a possible long-term option, explicitly not a current deliverable. No separate PRD exists.
* **Baseline:** Source inspected at `e94d1cfb4a469abc60b89b959407c7dfe00f6f63`, fetched `origin/main` on 2026-11-20.
* **Related work:** [CLI usability consolidation](plan-centaur-cli-usability.md) and [CLI README](../../crates/centaur-session-cli/README.md). Preserve their configuration, output, endpoint-binding and compatibility rules.
* **Verified — source:** `services/console/config/routes/api.rb` exposes operator-key principals, roles, role assignments and grants. `Api::BaseController` requires an API key owned by an active administrator on every request.
* **Verified — source:** `services/console/AGENTS.md`, `app/controllers/concerns/console_api_authentication.rb` and `config/routes/console_admin.rb` deliberately restrict users and API clients to Console sessions. Principal deletion is also Console-only. Console endpoints reject bearer credentials, including when accompanied by a cookie, and require CSRF plus same-origin Origin for writes.
* **Verified — source:** `Principal`, `Role`, `ApiClient`, `ConsoleUserPrincipalProvisioner` and their controllers define the lifecycle and identity caveats below. An API client's name is not a unique external identifier; its creator is not automatically its execution identity.
* **Verified — local observation:** The installed executable still has no administration group; it was not replaced. The new local build implements the supported operator-key commands and passes executable fixtures plus a real loopback CLI → Rails → disposable PostgreSQL workflow. See the [validation record](../../crates/centaur-session-cli/VALIDATION.md#console-administration--2026-11-20) for counts, tools and limitations.
* **Implementation status:** First-delivery command names/config/output are implemented below. Console-only commands and authentication expansion remain proposed and unimplemented; no security-policy expansion was approved.
* **Unknown:** The final human/automation authentication mechanism for Console-only administration. This blocks the corresponding phases, not the supported operator-key commands.

### Requirements and acceptance criteria

| ID | Requirement                                     | Acceptance criterion                                                                                                             |
| -- | ----------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| R1 | One executable, explicit administrative surface | Administration is under `centaur admin`; existing chat/integration behavior is unchanged.                                        |
| R2 | Separate credentials and destinations           | Admin calls never use chat secrets/tokens or send an operator key to an implicitly inherited endpoint.                           |
| R3 | Useful per-resource operations                  | Every command below has a documented HTTP mapping, prerequisites, failure behavior and runnable fixture coverage before release. |
| R4 | Preserve authorization boundaries               | Existing keys cannot reach Console-only operations; gated commands are absent until their authentication gate passes.            |
| R5 | Scriptable without pretending to be Terraform   | Stable IDs, explicit upsert, complete list pagination and secret-free JSON; no local state engine or provider.                   |
| R6 | Safe mutations                                  | Explicit destructive confirmation, no blind mutation retries and no credential plaintext in normal output.                       |
| R7 | Honest identity and permission semantics        | Principal creation does not imply a user account or least privilege; user admission is not invented by a client command.         |

## Architecture Overview

```text
centaur chat / integrations -> existing api-rs client and chat credentials
centaur admin              -> Console operator API and separate operator key
                            -> Console-only administration: gated, no fallback
```

Use the existing executable, not a second binary or generic SDK framework. Add one focused `src/admin.rs` module and expose only the existing small configuration/URL/secret-input helpers it actually needs. Do not put Rails administration in `CentaurClient`, whose current responsibility is the session API.

**Options considered:** (1) separate admin binary: unnecessary packaging overhead; (2) add a thin `admin` command group using supported APIs: preferred; (3) immediately add a declarative reconciler/provider or broaden every endpoint's authentication: unnecessary scope and, for the latter, contrary to current security policy.

Future-provider compatibility means preserving server-owned IDs, readable current state and explicit operation semantics. It does not require a provider-shaped CLI, a new schema registry, bulk apply, export/import, or a custom plan engine. A future provider should normally use the API directly. Current gaps, including principal deletion, API-client external identity and Console-only authorization, must remain explicit rather than being hidden by the CLI.

## Components

### Common command contract

All resource commands below are relative to `centaur admin`.

* `--json` emits exactly one JSON object; `list` uses `{ "items": [...] }`, `get` and successful writes use `{ "data": ... }`, and successful removals use `{ "ok": true }`. Diagnostics go to stderr. No partial list is emitted if a later page fails. HTTP 204 is handled without attempting JSON parsing.
* Human output includes resource IDs and the actual effect. An accepted mutation is not proof that running sessions/proxies have observed it.
* `list` follows Console's `page`/`limit`/`total_pages` pagination, not api-rs session cursors. Preserve unpaginated responses where that is the existing contract. Optional repeated `--label KEY=VALUE` maps only to supported label filters; `--limit` sets page size, not an undisclosed total-result cap. Listing is not a transactional snapshot.
* `<ref>` means an OID or, for principals/roles, a `foreign_id`. Resolve foreign IDs through canonical lookup routes, never fuzzy names. Writes documented as update-only resolve to an OID before mutation; an absent/deleted OID fails rather than creating a replacement.
* `--file PATH` or `--file -` supplies one JSON API document with its existing `{ "data": {...} }` envelope. Reject malformed input, unknown fields and conflicting selectors before networking. JSON only; no YAML parser or template language. Provide short sample documents in the README rather than dozens of field flags.
* For principals and roles, `update` changes supplied fields and replaces supplied collections according to the actual controller; omitted fields are retained. `upsert` does not mean full desired-state reconciliation or removal of omitted grants/roles. Do not generalize the secret-document replacement semantics in `Api::BaseController` to these resources.
* `delete`, role removal, grant deletion, user disable/demotion, API-client disable/detachment and secret rotation require `--yes`. Without it, explain the effect and send no mutation. This is a confirmation guard, not an accurate server-side dry run. Granting authority remains an explicit command, not an implicit login side effect.
* Do not automatically retry POST/PATCH/PUT/DELETE after ambiguous transport failure, even when some operations are logically idempotent. Report the ambiguity and the read operation to inspect state. Rotation/create-secret failures require special recovery, not replay.
* Use HTTPS, HTTP only on loopback for fixtures, no embedded URL credentials/query/fragment, no redirects, bounded timeouts and response/input sizes. Reject unsafe reference/path input and encode URL segments. Never echo authorization headers, secrets or raw credential-bearing bodies/errors.

**ADR Reference:** None for thin dispatch and existing API reuse. Authentication expansion is candidate A below.

### Operator setup and authentication subcommands — first delivery

| Subcommand                          | Proposed behavior                                                                                                               | Existing evidence / acceptance                                                                                                     |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `config set endpoint <console-url>` | Save a separate admin endpoint; changing it invalidates the saved operator key.                                                 | Reuse endpoint validation and atomic mode-0600 config storage. Never inherit the chat endpoint.                                    |
| `config show`                       | Show admin endpoint, its source and key presence only; no request.                                                              | Allowlisted fields, not raw config serialization.                                                                                  |
| `config unset endpoint`             | Remove admin endpoint and bound saved key; leave chat settings alone.                                                           | Removal is local, not server revocation.                                                                                           |
| `login [--key-stdin]`               | Read an operator API key from hidden input or stdin; validate with `GET /api/v1/roles?limit=1&page=1`; save only after success. | This proves operator authorization at that moment, not identity, least privilege or session readiness. No `/oauth/token` exchange. |
| `status [--json]`                   | Run the same non-mutating authorization check and report endpoint/auth mode; do not invent a who-am-I response.                 | A revoked key or inactive/non-admin owner must fail.                                                                               |
| `logout`                            | Remove only saved admin key/binding.                                                                                            | Existing root `logout` remains chat-only; external/environment keys are not revoked or removed.                                    |

Proposed config keys in the existing protected file: `admin-endpoint`, `admin-api-key`, `admin-credential-endpoint`. Add `CENTAUR_ADMIN_URL` and `CENTAUR_ADMIN_API_KEY` for secret-manager injection; their effective endpoint is explicit and independently validated. Environment values override saved admin values; a saved key is usable only for its exact bound endpoint. Reject explicitly supplied root `--api-url`/`--api-token` flags on admin commands with guidance to the admin settings rather than silently ignoring or reinterpreting them. Existing root chat environment variables must neither select nor authenticate administration.

Update the permission check in `load_config` to cover either saved credential type. Saving/removing an admin key must preserve chat settings and vice versa. Endpoint changes invalidate only the corresponding credential. Initially no key creation, rotation or revocation commands: obtain/revoke the operator key through the existing authorized operational path. Do not scrape bootstrap logs or add automatic privilege escalation.

### Principal subcommands — first delivery except deletion

| Subcommand                                                          | HTTP operation / behavior                                              | Acceptance and limitations                                                                                                                         |
| ------------------------------------------------------------------- | ---------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `principals list [--label KEY=VALUE] [--limit N] [--json]`          | `GET /api/v1/principals`, all pages.                                   | Stable OIDs and foreign IDs; do not claim the bearer payload contains Console-only identity fields.                                                |
| `principals get <ref> [--json]`                                     | GET by OID or `/principals/lookup/:foreign_id`.                        | Missing is a nonzero error, not an empty object.                                                                                                   |
| `principals create --file PATH [--json]`                            | `POST /api/v1/principals`.                                             | A duplicate fails; no implicit role assignment by the CLI. Server creation callbacks/defaults still apply.                                         |
| `principals update <ref> --file PATH [--json]`                      | Resolve existing OID, `PATCH /api/v1/principals/:oid`.                 | Update-only: no create-on-typo. Preserve omitted fields and explain replacement of supplied labels/permission collections.                         |
| `principals upsert <foreign-id> --file PATH [--json]`               | Direct `PUT /api/v1/principals/:foreign_id`.                           | No preflight get/create race. Reject contradictory body identity. Repeat preserves OID; do not claim it removes omitted permissions.               |
| `principals effective-config <ref> [--json]`                        | Existing OID/lookup `effective_config` GET.                            | Return only the server-redacted config; never call proxy `/sync`. Not a real upstream permission test.                                             |
| `principals roles list <principal-ref> [--json]`                    | `GET /principals/:principal_oid/roles`.                                | Includes actual assigned defaults; useful after creation.                                                                                          |
| `principals roles add <principal-ref> <role-ref> [--json]`          | Resolve OIDs; POST `{ "data": { "role_id": "..." } }` to nested roles. | Existing API supports repeated/racing assignment without duplicates.                                                                               |
| `principals roles remove <principal-ref> <role-ref> --yes [--json]` | DELETE nested role assignment by role OID.                             | Removes the assignment, not the role; missing assignment remains an error.                                                                         |
| `principals delete <ref> --yes [--json]`                            | **Gated:** existing Console DELETE only.                               | Do not add bearer deletion or substitute direct SQL. Explain cascaded grants/assignments/tasks/OAuth state and proxy unassignment before enabling. |

**Creation prerequisite:** Review deployment defaults through its authorized operator workflow before creating principals. `Principal#assign_default_roles` inherits all `assign_by_default` roles, and creation also applies sandbox defaults and credential reconciliation. A small input document is not evidence of least privilege, and reading roles after creation is not a preventive security control. The CLI must not silently strip inherited roles or auto-borrow infrastructure grants.

Generic principal creation does not create a Console user or safely establish personal ownership. Keep Console-owned provisioning with `ConsoleUserPrincipalProvisioner`; do not generate that service's foreign IDs or bind by display-name/email guesses. The initial principal document allowlist is `foreign_id` (create/upsert identity only), `name`, `labels` and the existing `sandbox_*` fields permitted by `PrincipalsController`. Exclude `kind`, `console_user_id` and all Slack identity/email fields; a dedicated personal-provisioning workflow requires a separately reviewed server operation.

### Role subcommands — first delivery

| Subcommand                                            | HTTP operation / behavior                  | Acceptance and limitations                                                                                   |
| ----------------------------------------------------- | ------------------------------------------ | ------------------------------------------------------------------------------------------------------------ |
| `roles list [--label KEY=VALUE] [--limit N] [--json]` | `GET /api/v1/roles`, all pages.            | Existing bearer response, not SPA-only summaries.                                                            |
| `roles get <ref> [--json]`                            | GET by OID or canonical foreign-ID lookup. | Names are not identifiers.                                                                                   |
| `roles create --file PATH [--json]`                   | `POST /api/v1/roles`.                      | Duplicate foreign ID fails.                                                                                  |
| `roles update <ref> --file PATH [--json]`             | Resolve OID, PATCH existing role.          | Must not silently create; supplied collections have their existing replacement behavior.                     |
| `roles upsert <foreign-id> --file PATH [--json]`      | Direct PUT by foreign ID.                  | Verify repeated and concurrent calls; do not assume principal retry handling exists for roles.               |
| `roles delete <ref> --yes [--json]`                   | Resolve OID, DELETE role.                  | Warn that all principal assignments and grants on that role are removed; principals/secrets are not deleted. |

Do not add `assign-by-default` management here: it is not in the current bearer role write contract and affects future principals globally. A role grant also does not grant Console-administrator status.

### Grant subcommands — first delivery

| Subcommand                                                             | HTTP operation / behavior                            | Acceptance and limitations                                                                                          |
| ---------------------------------------------------------------------- | ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `grants list --principal <ref> [--limit N] [--json]` or `--role <ref>` | Resolve grantee OID; GET nested `grants`, all pages. | Exactly one selector. Lists direct grants, not all effective inherited authority. No global grant index exists.     |
| `grants get <grant-oid> [--json]`                                      | `GET /api/v1/grants/:oid`.                           | Return existing reference-only payload.                                                                             |
| `grants create --file PATH [--json]`                                   | `POST /api/v1/grants`.                               | Require exactly one principal/role OID and exactly one supported secret OID. Repetition returns the existing grant. |
| `grants delete <grant-oid> --yes [--json]`                             | `DELETE /api/v1/grants/:oid`.                        | Revokes only that grant; another direct/inherited grant may still authorize access. Missing is an error.            |

Use the current eight secret-reference keys from `GrantsController::GRANTABLE_TYPES`; do not load secret plaintext or invent a second permissions model. No grant `update`: grantee/secret identity changes mean a separate explicit grant and revocation. The current server chooses the first present selector; add exact-cardinality validation on the server as well as the CLI before advertising rejection of ambiguous documents. Preserve valid existing requests and cover both credential namespaces that reuse this controller.

### User subcommands — gated delivery

**Gate:** Existing `/api/v1/console/users` requires a Console administrator session. An operator key or chat API-client token cannot be substituted. These commands are planned, not ready for implementation under the current credential policy.

| Subcommand                                | Existing Console action                                                 | Acceptance and limitations after the gate                                                                         |
| ----------------------------------------- | ----------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `users list [--status STATUS] [--json]`   | GET users collection; STATUS is pending, active or disabled.            | Current API is unpaginated. Preserve provider/status/admin metadata.                                              |
| `users get <user-oid> [--json]`           | Select exact OID from users collection; no show route currently exists. | Nonzero on missing; do not fuzzy-match names or implicitly select another account.                                |
| `users approve <user-oid> [--json]`       | POST `/:id/approve`.                                                    | Reuse approval service and approver metadata; document whether this also reactivates disabled accounts.           |
| `users disable <user-oid> --yes [--json]` | POST `/:id/disable`.                                                    | Preserve self-disable refusal and current MCP/session revocation behavior; test effects at their real boundaries. |
| `users promote <user-oid> [--json]`       | POST `/:id/promote`.                                                    | Explicit privilege grant; server also activates the user. Never infer promotion from execution-role membership.   |
| `users demote <user-oid> --yes [--json]`  | POST `/:id/demote`.                                                     | Preserve self-demotion refusal; test resulting operator-key rejection.                                            |

No `users create`, password reset, invite, delete or generic update in this delivery: these are not current user-controller operations. SSO admission/user provisioning and personal-principal provisioning are distinct. If adding users without a first login is required, first define the admission/identity contract and corresponding server API; do not simulate it with principal creation or write directly to Rails models from the client.

### API-client subcommands — gated delivery

**Gate:** Same Console-session requirement as users, plus `ApiClientsFeature.enabled?`. Feature-off 404 must not trigger a fallback or deployment change. These are session execution clients, not Console operator API keys or external OAuth-app registrations.

| Subcommand                                                                | Existing Console action                              | Acceptance and limitations after the gate                                                                         |
| ------------------------------------------------------------------------- | ---------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `api-clients list [--json]`                                               | GET collection.                                      | Current API is unpaginated; never includes the plaintext secret.                                                  |
| `api-clients get <client-oid> [--json]`                                   | GET member.                                          | Show scopes, disabled state and attached principals/default.                                                      |
| `api-clients create --file PATH --secret-out PATH [--json]`               | POST collection; creation issues a show-once secret. | Validate the protected output destination before sending; write the secret only there and exclude it from stdout. |
| `api-clients update <client-oid> --file PATH [--json]`                    | PATCH name/scopes only.                              | Only `sessions:read`/`sessions:write` currently exist; no admin scope invention.                                  |
| `api-clients disable <client-oid> --yes [--json]`                         | POST `/:id/disable`.                                 | Idempotent state change; verify new authentication and already-issued-token behavior separately.                  |
| `api-clients enable <client-oid> [--json]`                                | POST `/:id/enable`.                                  | Does not attach or grant a principal.                                                                             |
| `api-clients rotate-secret <client-oid> --secret-out PATH --yes [--json]` | POST `/:id/rotate_secret`.                           | Show-once output handling; no automatic retry, and do not promise existing access tokens are revoked.             |
| `api-clients delete <client-oid> --yes [--json]`                          | DELETE member.                                       | Explain credential/link removal; do not claim it kills running sessions or deletes principals.                    |
| `api-clients principals list <client-oid> [--json]`                       | Read member's `principals` array.                    | Include attachment/link OID and default flag.                                                                     |
| `api-clients principals attach <client-oid> <principal-ref> [--json]`     | POST `/:id/principals` with `data.principal`.        | Existing endpoint resolves principal OID/foreign ID; first attached principal becomes default.                    |
| `api-clients principals detach <client-oid> <link-oid> --yes [--json]`    | DELETE `/:id/principals/:link_id`.                   | Link must belong to that client; do not confuse it with the principal OID or silently select a new default.       |
| `api-clients principals set-default <client-oid> <link-oid> [--json]`     | POST `/:id/principals/:link_id/default`.             | Must reference an existing attached link, preserving one-default constraints.                                     |

`--secret-out` must create a new file exclusively with mode 0600 and refuse overwrite/symlinks; never accept `-` for stdout. Pre-open/check it before issuing a secret and close/remove only an empty unused file after pre-mutation failure. Flush/sync the issued secret before reporting success. A crash or network loss can still occur after server mutation: report uncertainty without retrying or claiming rollback. For lost create responses, inspect clients and explicitly recover/remove the orphan; for lost rotation output, perform a new explicit rotation to a fresh protected file. Never print the raw issuance response or attach it to an error report.

API-client names are not unique keys and there is no `foreign_id` in the inspected model. Therefore no client upsert-by-name or retry-safe create claim. A future provider needs a separate decision on imports/external identity. Personal MCP additionally depends on the client's actual `created_by` owner and matching personal default principal; an administrator creating a client must not be described as creating it on behalf of an arbitrary user.

## Implementation Order

| Phase | Component                                                  | Dependencies                                                  | Estimated Scope |
| ----- | ---------------------------------------------------------- | ------------------------------------------------------------- | --------------- |
| 1     | Baseline, admin parser/configuration/authentication/output | Existing operator API and authorized key provisioning         | M               |
| 2     | Principal/role reads and redacted effective config         | Phase 1                                                       | S               |
| 3     | Principal/role writes and assignments                      | Phase 2; review principal defaults before creation            | M               |
| 4     | Direct grants and ambiguity validation                     | Phase 3; existing secret OIDs                                 | S               |
| 5     | First-delivery regression checks and documentation         | Phases 1–4                                                    | M               |
| 6     | Console-only authentication decision and proof             | Explicit security-policy decision; separate design acceptance | L / gated       |
| 7     | Users and principal deletion                               | Phase 6                                                       | M / gated       |
| 8     | API clients, attachments and safe show-once output         | Phase 6; client feature enabled                               | M / gated       |
| 9     | Full gated-delivery acceptance                             | Phases 7–8                                                    | M / gated       |

Phases 1–5 are independently useful and need not wait for a provider or Console authentication expansion. This ordering does not authorize delegation, implementation, commits, publication, activation or deployment.

### Task 1: Establish the command boundary and operator setup

**Files:** Modify `crates/centaur-session-cli/src/main.rs`, `src/terminal.rs` and `README.md`; create `src/admin.rs` and `tests/cli_admin.py` inside that crate.

1. Add failing parser/loopback tests for admin help, rejection of conflicting root options, separate endpoint selection, protected config and credential isolation in both directions.
2. Run the new fixture against the baseline and confirm failure is due to the missing admin command, not a missing binary/environment.
3. Implement only the setup commands and a small Console HTTP helper. Share existing config/endpoint/hidden-input helpers, with explicit credential labels, rather than copying their security logic. No resource-generic SDK or new auth framework.
4. Extend permission/redaction tests to admin-only and mixed config files; cover environment overrides, revoked keys, inactive/non-admin owners, redirect refusal, timeout, oversized/malformed responses and later-page errors.
5. Verify `config show`, `logout` and both login paths preserve the other credential type. Update top-level help to describe chat and administration without claiming all planned groups are available.

### Task 2: Add principal and role reads

**Files:** `src/admin.rs`, parser tests in `src/main.rs`, new `tests/cli_admin.py`; existing server references `PrincipalsController`, `RolesController` and `PrincipalRolesController`.

1. Add failing fixtures for every read command in the principal/role tables, canonical lookups and multi-page responses.
2. Implement the exact route mappings, label encoding, strict refs and one JSON result contract.
3. Test OID/foreign-ID ambiguity, absent refs, malformed pagination, no partial JSON on failure and redacted effective-config output with synthetic secret canaries.
4. Confirm reads never create a principal, change a grant or call `/sync`.

### Task 3: Add principal/role writes and assignments

**Files:** `src/admin.rs`, `tests/cli_admin.py`; existing Rails tests `principals_controller_test.rb`, `roles_controller_test.rb` and `principal_roles_controller_test.rb` under `services/console/test/controllers/api/v1/`.

1. Add failing tests for create versus update-only versus direct upsert; include contradictory identities, forbidden account-linking fields and malformed/unknown document fields before any network request.
2. Implement supported writes and nested role commands; leave principal deletion absent.
3. Run existing Rails tests against a dedicated disposable Console database and add only missing contract cases: repeat/concurrent upsert, defaults applied on creation, collection replacement and deletion cascades. If role concurrency exposes an actual defect, repair/test the existing server upsert rather than masking it with a CLI read-before-create loop.
4. Test mutation ambiguity without retry, confirmation/no-request behavior and exact role/assignment IDs.
5. Document principal-default review, personal ownership limits and role-deletion effects.

### Task 4: Add direct grant operations

**Files:** `src/admin.rs`, `tests/cli_admin.py`; modify `services/console/app/controllers/api/v1/grants_controller.rb` for exact selector cardinality; extend `grants_controller_test.rb` and `grantee_grants_controller_test.rb`.

1. Add failing CLI and Rails tests for zero/multiple grantees and zero/multiple secret refs. Cover valid documents across every currently supported secret-reference key.
2. Add the minimal server validation before resolving or mutating any reference; keep Console subclasses using the shared validation.
3. Implement list/get/create/delete using existing endpoints. No global list, grant update or secret retrieval.
4. Verify repeated creation returns the same grant, direct lists are paginated, removal requires confirmation and inherited authority is not represented as revoked.

### Task 5: Validate and document the first delivery

**Files:** CLI `README.md`, `ARCHITECTURE.md`, `VALIDATION.md`; `services/console/docs/API.md` for stricter grant input validation; this plan's Progress section when implementation happens.

1. Run the local commands in Verification, recording actual results separately from planned acceptance.
2. Add a credential-free README walkthrough: configure admin endpoint, login through stdin, create/upsert a role and generic principal from small JSON documents, list actual roles, add an assignment, grant an existing secret OID and inspect effective config.
3. State that user/client provisioning and principal deletion remain unavailable; link the authentication gate instead of documenting fictitious working commands.
4. Record no provider/reconciler, no deployment changes and no proof of upstream permission enforcement from HTTP fixtures alone.

### Task 6: Resolve the Console-only authentication gate

**Files to inspect, not automatically change:** `services/console/AGENTS.md`, `app/controllers/concerns/console_api_authentication.rb`, `config/routes/console_admin.rb`, `config/routes/console_principals.rb`, `test/controllers/api/v1/credential_policy_test.rb`, existing login/OAuth services and credential policy docs.

1. Decide whether these commands need interactive human login only, unattended automation, or both. This is unresolved; do not silently choose automation or request broader server credentials.
2. Assess reuse of an explicitly supported human Console authentication flow versus a separately scoped native administrative authorization interface. No browser-cookie scraping, cookie import, password scraping or repurposing chat/MCP tokens as admin authority.
3. If introducing administrative bearer authority, obtain an explicit security-policy decision and record candidate ADR A before implementation. Define who can mint/use it, allowed actions, expiry/revocation, active-admin checks, bootstrap/recovery, audit attribution and credential storage. Existing operator keys must not automatically acquire the new authority.
4. Prove the chosen flow in local HTTP/PostgreSQL fixtures: non-admin/pending/disabled users rejected; revocation and namespace isolation enforced on actual requests; browser endpoints retain their session/CSRF policy.
5. Amend this plan with exact routes, config/login commands, affected files and runnable acceptance tests. Only then are Tasks 7–9 implementation-ready. A token minting proof alone does not pass the resource-authorization gate.

### Task 7: Add user lifecycle and principal deletion after the gate

**Known files:** CLI `src/admin.rs` and `tests/cli_admin.py`; Console `app/controllers/api/v1/console/users_controller.rb`, `app/controllers/api/v1/console/principals_controller.rb`, their route files, `test/controllers/api/v1/console_users_test.rb` and `console_principals_test.rb`. Additional auth files depend on Task 6.

1. Add failing CLI and real authorized-request tests for every command in the user table and principal deletion, including forbidden self actions and wrong credential classes.
2. Reuse existing server lifecycle methods/cascades; do not call Rails models from the client or broaden operator-key acceptance.
3. Verify account disablement/demotion at subsequent resource requests, and measure MCP/session revocation using actual consumers rather than only checking an updated user row. Inspect `User#approve!` before documenting reactivation semantics.
4. If an unattended credential has no attributable human admin identity, resolve self-protection and last-admin recovery on the server before enabling user mutations.

### Task 8: Add API clients and principal attachments after the gate

**Known files:** CLI `src/admin.rs`, `tests/cli_admin.py`; Console `app/controllers/api/v1/console/api_clients_controller.rb`, `app/models/api_client.rb`, `app/models/api_client_principal.rb`, `config/routes/console_admin.rb`, `test/controllers/api/v1/console_api_clients_test.rb`. Additional auth files depend on Task 6.

1. Add failing cases for every API-client command, feature-off behavior, unsupported scopes, wrong-client attachment IDs and duplicate names.
2. Implement protected secret-output handling first; test overwrite/symlink refusal before network traffic, response redaction, mode 0600, write failure, dropped issuance response and no automatic rotation retry.
3. Add CRUD/state and attachment/default commands without a parallel client registry or upsert-by-name.
4. Verify client auth/owner state and already-issued access-token behavior through api-rs, not only Rails state. Preserve personal-principal/creator ownership checks; do not infer clients can be created on behalf of another user.

### Task 9: Complete gated acceptance and handoff

**Files:** CLI docs and this plan; server auth/API docs and tests selected by Task 6.

1. Run all credential-policy and affected Rails/CLI suites with actual disposable database/HTTP fixtures; confirm tests did not skip for missing environment.
2. Exercise a local end-to-end operator flow without real external provider secrets: authorized login, user lifecycle, client issuance to protected file, attached principal selection, scope rejection, disablement and cleanup. Real model execution is not necessary for CRUD acceptance.
3. Record command availability, exact authorization modes, revocation limitations and ambiguous-secret recovery. Separate built, installed and deployed state.
4. Leave integration/publication/deployment to a separately authorized workflow. Do not mark this plan complete merely because phases 1–5 shipped.

## Verification

The first-delivery checks below have now run locally; exact results and pre-existing lint/version-gate failures are recorded in [VALIDATION.md](../../crates/centaur-session-cli/VALIDATION.md#console-administration--2026-11-20). Gated-phase checks remain planned. From the worktree root, with its development environment loaded:

```bash
cargo fmt --all -- --check
cargo test --locked -p centaur-session-cli
cargo build --locked -p centaur-session-cli
CENTAUR_CLI_TEST_BINARY="$PWD/target/debug/centaur-session-cli" \
  python3 crates/centaur-session-cli/tests/cli_configuration.py
CENTAUR_CLI_TEST_BINARY="$PWD/target/debug/centaur-session-cli" \
  python3 crates/centaur-session-cli/tests/cli_admin.py
```

`cli_admin.py` was added in Task 1 using the existing fixture's environment-variable convention; it is absent from the inspected baseline. If `CARGO_TARGET_DIR` is overridden, use the actual built executable path. Both fixtures must use only synthetic credentials and loopback servers.

For server changes, use the repository's documented Console development environment and a dedicated disposable PostgreSQL database, never a deployment database. Keep Rails fixtures separate from Rust Console-schema integration databases. From `services/console`, after explicit disposable database preparation:

```bash
PARALLEL_WORKERS=1 bin/rails test \
  test/controllers/api/v1/credential_policy_test.rb \
  test/controllers/api/v1/principals_controller_test.rb \
  test/controllers/api/v1/roles_controller_test.rb \
  test/controllers/api/v1/principal_roles_controller_test.rb \
  test/controllers/api/v1/grants_controller_test.rb \
  test/controllers/api/v1/grantee_grants_controller_test.rb \
  test/controllers/api/v1/console_users_test.rb \
  test/controllers/api/v1/console_api_clients_test.rb \
  test/controllers/api/v1/console_principals_test.rb \
  test/controllers/api/v1/sandbox_admin_controller_test.rb
bin/rubocop
bin/brakeman --quiet --no-pager --exit-on-warn --exit-on-error
```

Task 6 must add exact auth and api-rs token-behavior checks once the mechanism is selected; the above is not a substitute. Expected result is no regressions, no skipped required database checks, isolated credentials/routes and the command-specific acceptance cases above. Do not weaken the credential route sweep to make a CLI command pass.

For plan/documentation changes, validate/format this plan with specdocs, check local links/source paths and run `git diff --check`. The first implementation also caps lists at 8 MiB/10,000 pages and documents at 1 MiB, encodes dotted foreign IDs before Rails routing, rejects oversized credential input, retries server-local role uniqueness races once and validates grant selector counts before resolving records. No provider, software installation or remote mutation is included.

## Risks and Mitigations

| Risk                                                         | Likelihood | Impact | Mitigation                                                                                                                                 |
| ------------------------------------------------------------ | ---------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------ |
| CLI convenience erodes deliberate Console-only policy        | High       | High   | Explicit gated phase; no mixed credentials, fallback endpoints or automatic scope expansion.                                               |
| Operator key goes to a chat or substituted endpoint          | Medium     | High   | Separate admin destination and credential fields, exact endpoint binding, no redirects and bidirectional isolation tests.                  |
| New principal inherits powerful defaults/credentials         | High       | High   | Review deployment defaults before creation; expose actual roles/redacted effective config; never promise least privilege from input alone. |
| Client create/rotation succeeds but plaintext output is lost | Medium     | High   | Pre-open protected exclusive output, sync before success, no retries and documented explicit recovery.                                     |
| Duplicate client names mistaken for durable identities       | High       | Medium | OID-only client selection; no upsert-by-name or declarative convergence claim.                                                             |
| Incomplete revocation described as stopped work              | Medium     | High   | Test real token consumers; separate identity disablement, token validity and running-execution termination.                                |
| Concurrent CLI/session work collides in parser/config files  | Medium     | Medium | Dedicated worktree, recheck current source before implementation; minimal shared-helper changes and existing regression tests.             |

## Open Questions

* **Blocking only gated phases:** Must Console-only administration support unattended callers, humans, or both? Which explicitly approved authentication mechanism satisfies the current credential policy?
* **Separate product scope:** Is pre-login Console user creation/invitation actually required? If so, what identity/admission model should the server expose? It is not provided by existing user endpoints.
* **Before principal creation:** Which configured default roles/capabilities are acceptable? This is deployment policy, not a value the CLI can guess or silently overwrite.
* **Future only:** Does API-client management need a stable external identifier and owner-delegation operation before a provider is worthwhile? No schema change is proposed now.

## ADR Index

| ADR                       | Title                                                                                                                | Status                                              |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| Candidate A — not created | Native CLI authority for Console-only administrative operations without promoting existing operator/chat credentials | Pending security-policy decision; blocks phases 6–9 |

The single executable and command spelling are small reversible choices captured here, not separate ADRs. No ADR or authentication expansion is approved by this plan.

## Progress

* [x] Inspect current CLI, Console routes/controllers/models, tests and credential policy.
* [x] Define per-subcommand behavior, API mappings, sequencing, safety and unsupported operations.
* [x] Implement and verify phases 1–5 after user authorization; scope and remaining check failures are in the validation record.
* [ ] Resolve authentication/admission gates and amend gated tasks with exact contracts.
* [ ] Implement and verify phases 6–9 after their gates pass and implementation is authorized.

First delivery implemented locally on `feat/cli-administration`. No real credentials or deployment state changed; the installed CLI was not replaced. No commit or publication performed. Console-only phases remain gated, so this plan stays Draft.
