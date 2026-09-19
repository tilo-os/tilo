# Shared runtime MCP — todo

Goal: one generic runtime MCP for every framework app, proved by a real square-image crop and insertion into Writer. Photos/Writer are an acceptance scenario, never a runtime allowlist.

- [x] Locate the existing shared runtime, production broker, action directory, permissions, invocation lifecycle and content store.
- [x] Write implementation spec and plan; start subagent-driven implementation with independent review gates.
- [x] Audit every GUI app's entry point, framework integration and action publication. All 47 GUI entries reach the production `ui::App` publication path (41 default, 6 custom live handlers). The headless agent is a consumer, not a GUI publisher. Unmanaged launches remain disconnected; source coverage is not native execution qualification.
- [x] Complete the shared MCP adapter integration for discovery, query, resolution, invocation, status and cancellation. Native-discovered typed-argument encoding is fixed and independently reviewed in `021d910f6`; the real crop now succeeds. The native-discovered peer ID zero mismatch is fixed in the shared broker allocator and independently reviewed; 21 MCP/host and 30 broker tests passed.
- [x] Review and test the adapter: malformed requests, permissions, confirmations, stale instances, asynchronous results and cancellation ownership. The latest completed adapter run has 14 passing tests; final protocol corrections and additional coverage are tracked below.
- [x] Add the native runtime MCP entry point, configured app startup, bounded stdio framing and process cleanup. Eight host tests, executable smoke and independent review passed.
- [x] Complete automatic action discovery for ordinary framework apps. All 47 share registration, and the host supplies runtime connections generically, but native testing exposed that resources-only widgets lack default paged discovery/resolution. Discovery fix `d63f401c3` and lifecycle refresh repair `a8ac0484` are independently reviewed and verified by native9. The shared implementation retains the existing published resource directory and preserves bounded exact-parent extensions used by Code Editor, Calendar and Mail. No per-app MCP adapters or unbounded remote enumeration. Only 8 of 47 GUI apps currently override paged discovery; the other 39 use the new common fallback. Distinct instance publication is already tested.
- [x] Independently review the shared discovery fix. Its 14 focused tests and 38 existing directory/cache/bound-action tests pass; spec and quality review approved with no findings.
- [x] Test Calculator discovery and execution beyond Photos and Writer. Native9 invokes add-function and verifies the fresh graph resource; the 47-app source matrix remains separate from three-app native qualification.
- [x] Run the real MCP-only workflow: native9 discovers the image, crops 48×32 to 32×32, exports the edited content and inserts that exact returned content into Writer. All four positive operations, including Calculator, reach Succeeded.
- [x] Independently verify all 1,024 crop pixels, BLAKE3 content hash, and the exact image hash/dimensions in Writer reconstructed from two verified signed envelopes. Exact owned PIDs are absent after cleanup; immutable hashes match.
- [x] Complete independent whole-feature review and document connection setup, verified platform support and remaining portability gaps. Review accepted the shared architecture and native proof, with the following final fixes required.
- [x] Return JSON-RPC `-32602` for unknown tool names, preserving the request ID.
- [x] Reject explicit null request IDs before dispatch, including mutating requests.
- [x] Add same-room unauthorized resource-read and abnormal shutdown child-cleanup regressions.
- [x] Finish the consolidated fix wave, run focused tests and a rebuilt host smoke check, then obtain independent scoped re-review.
- [x] Deliver the working endpoint and evidence, ready for the user's additional test.

Constraints: YAGNI; shared code wherever possible; platform-specific code only at edges; no duplicate registry/permissions/workflow engine; no fabricated success or app-specific MCP routes. No remote federation until needed. Existing Unix host support must not be reported as Windows-native validation.

Implementation plan: [2026-09-17-shared-runtime-mcp.md](superpowers/plans/2026-09-17-shared-runtime-mcp.md).

Final verification: commit `d163e30d6`, 25 adapter/host tests passed, rebuilt executable smoke passed, and independent final scoped review approved. See [completion audit](superpowers/audits/shared-runtime-mcp-final/completion-audit.md).
