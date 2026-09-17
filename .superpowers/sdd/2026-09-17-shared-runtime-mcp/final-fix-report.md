# Shared runtime MCP — consolidated final fix report

Base was verified before editing as `5be005f022d8f4bf0194e0a089e06f6b15aea41b`. This wave owns only `shell/src/runtime_host/mcp.rs`, `mcp_tests.rs`, and `mcp_process_tests.rs`, plus this ledger and its retained logs. No App, widget, history, browser, broker, native-proof, or host-process production source was changed.

## Result

- I1 is closed. A parsed `tools/call` name absent from the canonical six definitions now returns JSON-RPC `-32602` with the original request ID and no `result`. The check derives from `tool_definitions()` instead of introducing another name registry. Known-tool argument parsing, execution, and broker failures still use MCP tool results with `isError: true`.
- I2 is closed. Request ID absence remains a silent notification boundary; explicit `id: null` and all other unsupported ID types fail the request validity check. Explicit null returns `-32600`, a null error ID, no result, and cannot reach tool dispatch. String and integer IDs remain intact.
- M1 is closed by a real same-room outsider `resources/read`. It passes live-peer selection, crosses the real broker bridge, and returns the currently intended authority-filtered page with no rows and no protected `workspace:atlas` content.
- M2 is closed by a real owned `/bin/sh` child and an injected MCP output failure. The test waits until the child records its exact PID, triggers the output failure, verifies `kill(pid, 0)` reports `ESRCH`, then reacquires the same profile with `Session::start`.
- M3 remains unchanged: the existing unused `next_wake`, unused test timeout setter, and macOS compact-unwind warnings are retained and visible in the Rust logs. No unrelated warning cleanup was attempted.

## TDD and regression evidence

Protocol RED command:

```text
cargo test -p shell --lib runtime_host::mcp::tests:: --no-default-features
```

`final-fix-adapter-red.log` records 14 passing tests and exactly the two new expected failures: the unknown-tool reply lacked `error.code == -32602`, and the explicit-null invocation reply lacked `error.code == -32600`.

After the minimal request-boundary change, the same command passed 16/16. `final-fix-adapter-green.log` retains that GREEN run.

The first abnormal-shutdown test run, retained as `final-fix-host-test-red.log`, showed that `serde_json::to_writer` wraps the injected writer's `BrokenPipe` as `io::ErrorKind::Other`. This was a test-harness expectation, not a cleanup failure. The test was corrected to identify the injected failure by its preserved message while keeping the required PID and profile assertions. No production host change was needed. The host GREEN command was:

```text
cargo test -p shell --lib runtime_host::mcp_process::tests:: --no-default-features
```

`final-fix-host-green.log` records 9/9 passing, including the new output-failure cleanup test.

## Final verification

Combined adapter and host command:

```text
cargo test -p shell --lib runtime_host::mcp --no-default-features
```

`final-fix-combined-green.log` records 25 passed, 0 failed. This is the final test run after tightening the M1 assertion to the actual filtered-empty response.

Updated binary command:

```text
cargo build -p shell --bin qos-runtime-mcp --no-default-features
```

`final-fix-build.log` records exit 0. The existing `target/debug/qos-runtime-mcp` was then run with a fresh short `/tmp/qmcp-fix-*` profile, a generated `{"apps":[]}` manifest, and newline-delimited initialize and ping requests. `final-fix-zero-app-smoke.log` records host status 0, validation status 0, exactly two stdout JSON replies with IDs 1 and 2, negotiated protocol `2025-06-18`, and ping result `{}`.

The first smoke harness used Python's default macOS temporary root and correctly hit the already documented Unix socket constraint, `path must be shorter than SUN_LEN`, before protocol handling. That environmental attempt is retained in `final-fix-zero-app-smoke-long-path.log`; the qualifying smoke used the required short isolated path. Native9 was not repeated because this wave does not change normal known-tool dispatch or native workflow semantics, and the brief explicitly retains the accepted Native9 proof.

The Python MCP proof suite was not rerun because no Python harness or proof source changed; the brief makes that rerun conditional on touching the harness. Existing native and Python evidence remains accepted by `final-review.md`.

Scoped source hygiene command:

```text
rustfmt --edition 2024 --check shell/src/runtime_host/mcp.rs shell/src/runtime_host/mcp_tests.rs shell/src/runtime_host/mcp_process_tests.rs
git diff --check -- shell/src/runtime_host/mcp.rs shell/src/runtime_host/mcp_tests.rs shell/src/runtime_host/mcp_process_tests.rs
```

Both exited 0; `final-fix-format-diff.log` is intentionally empty.

## Self-review

- The protocol distinction is made before `McpServer::call`, so an unknown tool cannot become a successful JSON-RPC envelope and explicit null cannot dispatch a mutation.
- The unknown-name guard reads the advertised definitions. The dispatch match remains the existing implementation, avoiding an additional divergent registry or broader refactor.
- Existing-tool invalid argument and broker failure coverage still asserts `result.isError == true`.
- The null-ID test uses the real broker fixture, drains any queued bridge work, and asserts `resolve_count() == 0`; a false positive based only on response shape cannot pass.
- The same-room read uses a visible root in the correct room. Its empty page therefore comes from current broker authority filtering rather than the different-room live-peer shortcut.
- The cleanup test performs no process-global environment mutation and runs inside the existing isolated subprocess setup. It targets only the child PID it launched and relies on `OwnedChildren` drop behavior already present in production.
- `git diff --check` is clean. The scoped diff contains no production change outside `mcp.rs`, no broad formatting, no test-only seam, and no unrelated source.

Commit subject: `fix(runtime): enforce MCP request boundaries`. The exact commit containing this report is recorded in the final task handoff.
