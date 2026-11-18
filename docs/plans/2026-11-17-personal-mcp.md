---
title: Personal MCP via native Console integrations
status: in-progress
---

# Personal MCP connections

Use Console OAuth apps, owned broker credentials and existing grants. The earlier parallel session-database registry and browser-to-session-API CRUD bridge are superseded. Existing inbound Centaur MCP service is not the outbound Codex client.

1. **Configuration:** operator opt-in on existing non-shared integrations. CLI recognizes a registration and links to existing consent. Browser disconnect requires personal session authentication and CSRF; machine session scopes cannot delete credentials.
2. **Session bootstrap:** snapshot non-secret endpoints for a matching personal principal at new-session creation. Retain operator overlay and snapshots on resume. Use Codex native transport; no credential fields in the overlay.
3. **Validation:** run Rust, PostgreSQL, Rails, frontend and CLI tests. Browser tests use synthetic state, not provider traffic. See the [CLI guide](../../crates/centaur-session-cli/README.md).
4. **Open release gates:** native Codex traffic through iron-proxy; effective destination/redirect restrictions and overlapping direct/role-granted credentials; account/provider isolation; refresh ownership, rotation, revocation and disablement with active sessions. Verify actual deployment ingress and overnight reconnect separately. No production mutation or deployment is part of local verification.
