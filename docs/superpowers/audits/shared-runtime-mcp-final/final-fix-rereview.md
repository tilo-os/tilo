### Finding Verdicts

- **I1 — Unknown tool names have the wrong protocol response** — ADDRESSED. `shell/src/runtime_host/mcp.rs:156-165` checks parsed names against the canonical `tool_definitions()` list before dispatch and returns JSON-RPC `-32602` with the original ID; `shell/src/runtime_host/mcp_tests.rs:32-49` proves the preserved string ID and absence of `result`. Known tools still enter `self.call` and retain tool-result wrapping at `mcp.rs:162`, so their argument, execution, and broker failures remain `isError` results.
- **I2 — Explicit null request IDs are accepted and dispatched** — ADDRESSED. `shell/src/runtime_host/mcp.rs:86-107` distinguishes an absent ID from explicit null, rejects null as `-32600` with a null error ID before method dispatch, and preserves silent notifications plus integer/string IDs. `shell/src/runtime_host/mcp_tests.rs:51-99` exercises a null-ID `qos_invoke` through the real fixture and proves `resolve_count() == 0`, then checks the notification and valid-ID boundaries.
- **M1 — Same-room unauthorized resource-read coverage is missing** — ADDRESSED. `shell/src/runtime_host/mcp_tests.rs:185-210` uses a visible peer in the same room with actor `outsider`, drains the real broker bridge, parses the returned resource page, and proves its rows are empty and protected `workspace:atlas` content is absent.
- **M2 — Failure-path cleanup lacks a surviving-child regression** — ADDRESSED. `shell/src/runtime_host/mcp_process_tests.rs:313-352` starts a real owned child, waits for its exact PID, injects an MCP output failure, verifies that PID is gone with `ESRCH`, and reacquires the same profile. The test exercises the existing `OwnedChildren` drop path without production lifecycle changes.

### New Breakage in the Fix Diff

None.

### Out-of-Scope Observations

None. M3's pre-existing compiler/linker warnings remain deliberately retained and visible in the supplied logs.

### Checks

- Inspected the supplied fix package for `5be005f022d8f4bf0194e0a089e06f6b15aea41b..d163e30d612b1c2f93d53de1511741c91bd4c6c3` and the three changed source files.
- Confirmed retained GREEN evidence: adapter 16/16, host 9/9, combined 25/25, build exit 0, and zero-app initialize/ping smoke exit 0.
- Confirmed the controller's independent executable smoke: initialize/ping, unknown tool `-32602` with preserved string ID and no result, null-ID invoke `-32600` with no result, silent initialized notification, four JSON replies, exit 0.
- No suite rerun or native launch was needed; the supplied focused evidence answers the changed protocol and cleanup paths.

### Verdict

**Fix round: All findings addressed, no new Critical/Important breakage.**
