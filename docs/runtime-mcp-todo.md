# Shared runtime MCP — todo

Goal: one generic runtime MCP for every framework app, proved by a real square-image crop and insertion into Writer. Photos/Writer are an acceptance scenario, never a runtime allowlist.

- [x] Locate the existing shared runtime, production broker, action directory, permissions, invocation lifecycle and content store.
- [x] Write implementation spec and plan; start subagent-driven implementation with independent review gates.
- [x] Audit every GUI app's entry point, framework integration and action publication. All 47 GUI entries reach the production `ui::App` publication path (41 default, 6 custom live handlers). The headless agent is a consumer, not a GUI publisher. Unmanaged launches remain disconnected; source coverage is not native execution qualification.
- [ ] Complete the shared MCP adapter integration for discovery, query, resolution, invocation, status and cancellation. Native testing exposed missing canonical typed-argument encoding; shared adapter repair is in progress. The native-discovered peer ID zero mismatch is fixed in the shared broker allocator and independently reviewed; 21 MCP/host and 30 broker tests passed.
- [x] Review and test the adapter: malformed requests, permissions, confirmations, stale instances, asynchronous results and cancellation ownership. Independent review fix verified; 12 adapter and 19 broker tests passed. Deferred minor coverage/protocol findings remain tracked for final review.
- [x] Add the native runtime MCP entry point, configured app startup, bounded stdio framing and process cleanup. Eight host tests, executable smoke and independent review passed.
- [x] Complete automatic action discovery for ordinary framework apps. All 47 share registration, and the host supplies runtime connections generically, but native testing exposed that resources-only widgets lack default paged discovery/resolution. Shared framework fix is implemented in `d63f401c3` and independently reviewed: it retains the existing published resource directory and preserves bounded exact-parent extensions used by Code Editor, Calendar and Mail. No per-app MCP adapters or unbounded remote enumeration. Only 8 of 47 GUI apps currently override paged discovery; the other 39 use the new common fallback. Distinct instance publication is already tested.
- [x] Independently review the shared discovery fix. Its 14 focused tests and 38 existing directory/cache/bound-action tests pass; spec and quality review approved with no findings.
- [ ] Test Calculator discovery and execution beyond Photos and Writer; retain an all-app coverage matrix, with missing evidence explicitly incomplete.
- [ ] Run the real MCP-only workflow (waiting for reviewed typed-argument transport fix): discover image → crop to 1:1 → export edited content → insert that returned content into Writer.
- [ ] Independently verify square image pixels, content hash, Writer document/image state and authenticated signed document evidence from the real Writer session.
- [ ] Complete independent final review and applicable regression tests; document connection setup, verified platform support and remaining portability gaps.
- [ ] Deliver the working endpoint and evidence, ready for the user's additional test.

Constraints: YAGNI; shared code wherever possible; platform-specific code only at edges; no duplicate registry/permissions/workflow engine; no fabricated success or app-specific MCP routes. No remote federation until needed. Existing Unix host support must not be reported as Windows-native validation.

Implementation plan: [2026-09-17-shared-runtime-mcp.md](superpowers/plans/2026-09-17-shared-runtime-mcp.md).
