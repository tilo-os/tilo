# Task 2 independent review

## Verdict

- **Spec Compliance: PASS.** The retained `native-3` evidence satisfies the Task 2 behavioral acceptance gate. It records literal ordinary Cargo launches for Writer, Files, Photos, and late Calculator; the first app exposes daemon PID 28409 before the MCP proxy starts; all four apps register as distinct peers; Calculator's freshly resolved `add-function` invocation reaches canonical `succeeded`; Writer exit removes only Writer while the other apps, daemon, and proxy remain live; and cleanup records all six exact known PIDs absent. The isolated runtime profile is absent now. Source snapshots match before the host build, before the proof, and after the proof, while exact exercised executable hashes are retained. The documentation accurately separates automatic app joining from explicit assistant `--connect`, preserves manifest mode, explains lifecycle/bootstrap/profile behavior, and limits the native claim to the observed macOS background behavior rather than visible frames, performance, Photos History, all 47 GUI apps, or Windows. `native-1` and `native-2` contain `failure.txt` and no success marker, so they are not acceptance evidence.
- **Task Quality: NEEDS FIXES.** The passing evidence is sound, but the committed workflow has two P2 defects that should be fixed before treating the proof runner as a reusable acceptance tool.

## Findings

### [P2] The documented proof command requires an artifact it never explains how to create

**Files:** `docs/runtime-mcp.md:150`, `scripts/runtime-auto-attach-proof.py:629`

The documented sequence says to build the host and then run the proof with a mandatory `--host-source-snapshot`, but neither the documentation nor the script provides a command or mode that creates that JSON in the required schema. The retained run has a valid snapshot and it exactly matches `source-hashes-before.json`, so this does not invalidate `native-3`; it does make the published proof instructions non-reproducible without importing private script internals or hand-authoring JSON. It also omits the ordering needed for the provenance claim: capture the source snapshot first, then build the host, then run the proof. Add a supported snapshot/preparation command (or make the runner capture the snapshot and perform the host build itself) and document the exact sequence.

### [P2] A proof can pass even when the temporary runtime profile was not removed

**File:** `scripts/runtime-auto-attach-proof.py:616`

The runner calls `shutil.rmtree(profile, ignore_errors=True)` and never checks that the profile is gone before writing `proof.json`. A permissions or filesystem failure can therefore leave runtime authority material behind while the run still reports success, contrary to the binding requirement to clean up the temporary profile. The retained `native-3` profile `/tmp/qos-auto-1omj8pct` is absent, so the current acceptance evidence is not affected. Remove the profile without suppressing errors, or explicitly fail the success gate when the path still exists; retain cleanup diagnostics on failure.

## Evidence checked

- Owned commit `e01fc4809c16c8347e93affc4d11270fd95d2b5b` and its parent-to-commit diff only.
- Binding spec and Task 2 brief/report.
- `native-3` proof, metadata, transcript, action, process, binary/source-hash, log, survivor, and cleanup artifacts.
- Earlier `native-1`/`native-2` failure markers and cleanup artifacts.
- Current absence of every PID listed in `cleanup.json` was reported by the controller; the retained profile path was independently observed absent during this review.
- No suites were rerun, per review instructions. The controller's fresh six-test unit run exited 0.

## Cross-task scope

No unresolved Task 2 compliance issue depends on visible-frame/performance qualification, Photos History acceptance, Browser privacy behavior, or actual native execution of all 47 source-audited GUI entry points. Those remain separate scopes. Controller-owned whole-change review and final acceptance audit remain open as recorded in the todo.
