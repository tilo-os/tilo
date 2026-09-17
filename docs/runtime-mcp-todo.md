# Shared runtime MCP — todo

Goal: one generic runtime MCP for every framework app, proved by a real square-image crop and insertion into Writer. Photos/Writer are an acceptance scenario, never a runtime allowlist.

- [x] Locate the existing shared runtime, production broker, action directory, permissions, invocation lifecycle and content store.
- [x] Write implementation spec and plan; start subagent-driven implementation with independent review gates.
- [x] Audit every GUI app's entry point, framework integration and action publication. All 47 GUI entries reach the production `ui::App` publication path (41 default, 6 custom live handlers). The headless agent is a consumer, not a GUI publisher. Unmanaged launches remain disconnected; source coverage is not native execution qualification.
- [ ] Implement the shared MCP adapter for discovery, query, resolution, invocation, status and cancellation. **In progress.** Reuse existing broker authority and typed action contracts.
- [ ] Review and test the adapter: malformed requests, permissions, confirmations, stale instances, asynchronous results and cancellation ownership.
- [ ] Add the native runtime MCP entry point, configured app startup, bounded stdio framing and process cleanup.
- [ ] Verify all framework apps use the same automatic publication path; remove any app-specific runtime restriction and fix shared startup gaps identified by the audit. Verify multiple instances remain distinct.
- [ ] Test MCP operation discovery and execution beyond Photos and Writer; retain an all-app coverage matrix, with missing evidence explicitly incomplete.
- [ ] Run the real MCP-only workflow: discover image → crop to 1:1 → export edited content → insert that returned content into Writer.
- [ ] Independently verify square image pixels, content hash, Writer document/image state and persisted or signed document evidence.
- [ ] Complete independent final review and applicable regression tests; document connection setup, verified platform support and remaining portability gaps.
- [ ] Deliver the working endpoint and evidence, ready for the user's additional test.

Constraints: YAGNI; shared code wherever possible; platform-specific code only at edges; no duplicate registry/permissions/workflow engine; no fabricated success or app-specific MCP routes. No remote federation until needed. Existing Unix host support must not be reported as Windows-native validation.

Implementation plan: [2026-09-17-shared-runtime-mcp.md](superpowers/plans/2026-09-17-shared-runtime-mcp.md).
