# Final whole-feature review — shared runtime MCP

Reviewed supplied `final-review.diff`, BASE `7ffdebec0b495d3f9efd36a3aba30871ab5ff136` through HEAD `5be005f022d8f4bf0194e0a089e06f6b15aea41b`. This verdict covers the complete owned feature package, including both disjoint App patches; it does not qualify concurrent Browser/History/NTFS/performance work.

## Spec compliance

The intended architecture and native outcome are satisfied: one generic adapter over the existing Session/broker, host-owned actor/workspace, current authority and atomic re-resolution, canonical typed arguments and terminal outputs, no approval boolean or mutation replay, and a thin manifest-configured Unix host. Shared App publication retains inert revision-bound metadata rather than another execution registry; ordinary declarations, exact-provider extensions, invalidation, ownership and normal idle refresh are covered. The retained source audit establishes the common integration path for 47 runtime-managed GUI entrypoints, separately from three-app behavioral evidence.

Native9 establishes the real 48×32 → 32×32 Photos crop, all 1,024 RGBA pixels, BLAKE3 export, the identical hash in independently reconstructed signed Writer state, and fresh Calculator graph discovery. Exact retained PIDs are absent and host exit is zero. This is background behavior on the locked Mac, not visible presentation, Windows-native support, all-47 native qualification, client installation or performance qualification.

Protocol correctness is not yet complete: I1 and I2 below should be fixed before delivery. No additional architectural deviation was found. The documented Unix transport limitation and shared shell linkage are justified explicit scope decisions, not waived requirements.

## Strengths

- `shell/src/runtime_host/mcp.rs:226`: invocation passes the existing typed-argument envelope through the production broker and preserves ActionOutput/InvocationUpdate. Real round-trip and native negative evidence support the implementation.
- `ui/src/app/action_directory_snapshot.rs:20`: the projection contains data only; current witness changes evict it and cursors. Provider overlap retains published contracts, while missing direct actions/children remain discoverable.
- `ui/src/app.rs:5191`: normal idle repairs evicted publication independently of a remote query. Connected lifecycle regressions cover the previously observed post-crop and presence gaps.
- `shell/src/runtime_host/mcp_process.rs:251`: configured children launch through Session, stdio is separated from app logs, bounded framing/queueing is explicit, and RAII drops children before services/profile ownership.
- `shell/examples/runtime-mcp-inspect.rs:96`: independent signed route validation and Automerge reconstruction, together with the pixel oracle, prevent status-only success claims.

## Issues

### Critical (Must Fix)

None found.

### Important (Should Fix before delivery)

**I1 — Unknown tool names have the wrong protocol response.**

- File: `shell/src/runtime_host/mcp.rs:152` (fallback at `:267`).
- Every successfully parsed tools/call wraps dispatcher failure with `tool_result`. An unknown name therefore returns a successful JSON-RPC envelope containing `isError: true`.
- MCP explicitly classifies unknown tools as protocol errors, with `-32602`; clients must be able to distinguish a nonexistent tool from failure of an existing operation. This also implements the binding ledger ruling.
- Reject unrecognized tool names before the tool-result wrapper, returning JSON-RPC `-32602`. Keep existing-tool validation/execution/broker failures as tool results. Add a focused unknown-name regression with an unchanged request ID and no result field.
- Source: [MCP 2025-06-18 tools error handling](https://modelcontextprotocol.io/specification/2025-06-18/server/tools#error-handling).

**I2 — Explicit null request IDs are accepted and dispatched.**

- File: `shell/src/runtime_host/mcp.rs:86`.
- `valid_id` accepts null, and only an absent id is treated as a notification. Thus `{ "jsonrpc":"2.0", "id":null, "method":"tools/call", ... }` can reach a mutating tool and receive an ordinary result with a null ID.
- MCP requires request IDs to be strings or integers and explicitly forbids null. This is a missing malformed-request boundary, not a permission bypass, but malformed requests must not dispatch mutations under an invalid correlation ID.
- Distinguish absence from explicit null; retain silent valid notifications, and return Invalid Request (`-32600`, null error ID) for explicit null before dispatch. Add coverage proving null-ID invocation never enters the broker, alongside a valid no-ID notification and string/integer request.
- Source: [MCP 2025-06-18 base protocol requests](https://modelcontextprotocol.io/specification/2025-06-18/basic/index#requests).

### Minor (Nice to Have; nonblocking)

**M1 — Same-room unauthorized resource-read coverage is missing.**

- File: `shell/src/runtime_host/mcp_tests.rs:116`.
- The test covers an authorized read, a different-room read, and a same-room outsider invocation. It never executes same-room outsider `resources/read`; the different-room branch exits at live-peer filtering before broker authority.
- The implementation does delegate the read to the real authorized query path, so this is a missing regression, not an observed disclosure. Add the same-room outsider read through the real bridge and assert no protected row/content is returned. Desirable in the consolidated wave; not independently merge-blocking.

**M2 — Failure-path cleanup lacks a surviving-child regression.**

- File: `shell/src/runtime_host/mcp_process_tests.rs:215` (production cleanup at `mcp_process.rs:222`, launch loop at `:251`).
- EOF tests check exact PID disappearance, and early exit checks idle termination. No focused test proves an already running child is reaped when output fails, input fails, or a later app fails to launch.
- RAII and drop order look correct, and native9 proves normal EOF cleanup; no leak is demonstrated. Add a small failure-path test retaining the exact first-child PID, injecting an I/O or later launch error, and checking child absence and profile re-acquisition. This improves confidence in the documented abnormal shutdown promise without requiring another native GUI run.

**M3 — Existing compiler/linker warning noise remains.**

- File: `ui/src/animation_schedule.rs:30`; evidence `.superpowers/sdd/2026-09-17-shared-runtime-mcp/task-6-final-tests.log:1`, `:11`, `:19` and `target/runtime-proof/mcp-native9/inspector-tests.log:1`.
- Unused `next_wake`, the existing test timeout setter and macOS unwind warning remain. They do not invalidate the passing checks. Preserve the qualification; do not expand this wave into unrelated cleanup.

## Deferred-finding triage

1. Unknown tool response: promoted to required I1, matching the existing ruling.
2. Same-room unauthorized resources/read: retained nonblocking M1; other-room coverage is not substituted for it.
3. Invalid typed arguments: closed. Actual broker delivery preserves the canonical envelope and values, and native9 observes an integer crop argument reach terminal Failed through real app validation. No longer a missing-evidence finding.
4. Surviving-child cleanup on abnormal exits: retained nonblocking M2; source RAII and normal shutdown evidence are not misrepresented as focused failure evidence.
5. Pre-existing warnings: retained M3, explicitly outside cleanup scope.

## Checks and named cross-cutting risk checks

Read the entire 4,948-line package in successive ranges; the first oversized tool result truncated the proof orchestration, so the missing portion was recovered from the same package. No second broad diff, unrelated source crawl, git mutation, native relaunch or test-suite rerun was performed. Read spec, ledger rulings/deferred findings, relevant task reviews/reports and retained test/evidence outputs.

- Query continuation/budget integration: inspected current `App::action_directory_page` entry clamps and revision/cursor identity checks (`ui/src/app.rs:2496`). New projected and provider stages share the existing result/byte/work limits.
- Exact-provider fallback: inspected `resolve_publication_resource` (`ui/src/app.rs:3012`); bound/unbound exact resolvers and generation-bearing search providers remain in use, without a new bulk resources fallback.
- Refresh sequencing: inspected `pump_collab_presence` (`ui/src/app.rs:5081`); callbacks are serviced before the regular event batch, and idle refresh repopulates invalidated state. Transient stale reads are bounded and mutations are not retried.
- Authority on projected rows: inspected `permits_directory_row` (`shell/src/shell/collab_broker_capnp.rs:5094`); descriptor consistency and current actor/workspace contract checks still gate both containers and actions.
- Session composition/drop boundary: inspected `Session::start`/`launch` (`shell/src/runtime_host.rs:43`, `:105`); existing persistent identity/workspace/brokers and shared blobstore remain authoritative.
- Protocol source check: verified official unknown-tool and request-ID rules; also checked progress-token rules, which correctly require string/integer and permit omitted progress notifications. No progress-token finding.

Retained evidence inspected includes 14 passing adapter tests plus focused broker lifecycle test, 18 passing connected projection tests (reports retain 38 existing regressions), 21 Python proof/legacy tests, 2 inspector tests, and the host report's 8 passing lifecycle checks/build/zero-app smoke. Native9 proof and cleanup artifacts match the accepted task review. No new execution was needed to establish I1/I2, whose branches are directly visible.

## Assessment

**Spec verdict:** Substantially compliant; fix I1/I2 for the advertised MCP contract.

**Quality verdict:** Shared architecture and native evidence approved; two localized protocol boundary defects remain.

**Ready to merge/deliver? With fixes.** The native acceptance outcome is real and the feature stays within the shared-runtime design. Complete I1/I2 in the single consolidated final fix wave, optionally close M1/M2 there, preserve M3, then perform exactly one scoped fix-wave re-review under the SDD rule.
