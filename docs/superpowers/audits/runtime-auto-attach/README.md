# Shared automatic runtime attachment acceptance

The source implementation is shared by all47 inventoried GUI entry points, including six custom live handlers. The source inventory is not a native execution claim for all47. Ordinary app startup calls the shared attachment helper before construction; no app allowlist was added.

Implementation commits: `c2d6045c8`, `6dce082e8`, and `2acfd7d8c`. Independent Task1 review found forced-offline bootstrap and stale ancestor descriptor metadata; both were fixed and passed scoped review. Focused checks: runtime-portal14, shared UI launch32, runtime host36; actual host build succeeded. Prior unrelated UI/compiler/shell lint warnings are disclosed in the task report.

## Native proof

On macOS, `target/runtime-proof/runtime-auto-attach/native-3/proof.json` verifies separate literal `cargo run -p word-processor`, `cargo run -p file-explorer`, `cargo run -p photos -- --open FIXTURE`, and late `cargo run -p calculator` launches. Writer starts the absent daemon before the MCP proxy connects. All four have distinct peers in one runtime. Calculator `add-function` reaches canonical `succeeded`. After Writer exits, Files, Photos, Calculator, runtime and MCP remain connected. All six owned PIDs are absent after cleanup, MCP exits0, and source hashes are unchanged.

The copied native JSON files retain command lines, identities, results, executable hashes and cleanup. Full transcripts/logs and before/after source hashes remain under the native-3 evidence directory. Native1 and native2 are retained failures in the test harness cleanup and are not acceptance evidence. Native3 uses exact owned PID termination plus child reaping; a real harmless subprocess regression covers this boundary.

This is four-app macOS behavior, not frame-time qualification, Windows support, external assistant installation, or the concurrent Photos History/Browser UX acceptance. Unix/macOS/Linux host edges are implemented; Linux execution has not been qualified by this run. Browser private-mode isolation has separate independently reviewed evidence in ../2026-09-17-browser-private-runtime.md.

## Requirement checks

| Requirement | Evidence |
| --- | --- |
| Shared by every normal GUI entry point | 47-entry source inventory; default/custom/definition launch tests |
| First plain app launch starts runtime | Native3 first Writer launch before proxy |
| Separate and late app joins | Native3 Files/Photos/late Calculator, four peer IDs |
| One existing MCP/action authority | Production Session/McpServer; real Calculator terminal success |
| Independent lifecycle | Writer exit leaves other3/runtime/proxy alive; host tests cover proxy EOF |
| Genuine inherited qOS transfer preserved | Portal precedence/malformed tests; owner-provenance fix review |
| Ordinary child can reconnect | Real SDK parent/child same-app test; command-handler resume regression |
| No manual host build required when missing | Serialized source-bootstrap regression; normal Cargo settings regression |
| Private/local/diagnostic boundary | Launch tests and separately reviewed Browser private isolation |
| Race/stale endpoint/private path handling | Host process tests and startup ownership review |
| Reproducibility and owned cleanup | Native3 command/source/binary/PID artifacts,8 proof-harness tests |
| Setup/platform limits documented | docs/runtime-mcp.md |

Task2 passed independent review after fixes for supported pre-build source capture and enforced temporary profile removal (`9c6de88b3`). The original native3 result remains valid; its profile was independently confirmed absent. Final whole-feature review found one configured-Cargo-target output discovery issue. Commit `2acfd7d8c` resolves it using the actual Cargo executable artifact, with deadline-bounded output collection and validated reuse. All14 portal tests and production portal Clippy pass; scoped final review is clean. Native3 predates this narrow missing-host bootstrap correction and remains the observed four-app behavior proof. The new configured-target branch has separate subprocess regression evidence.


## Workspace recovery

The final-fix worker accidentally invoked workspace-wide Cargo formatting. No unrelated formatting was staged or committed. Recovery preserved a full current diff and file copies, then restored218 paths from verified prior hashes, two more from token-level reconstruction matching prior hashes, and23 from existing Git blobs matching prior hashes. Every recovery also required that formatting the recovered bytes reproduced the current bytes exactly. The shared launch file was restored from the History owner’s exact approved snapshot. Browser recovered its three active files from its own known bases; Photos/ImageViewer were already unchanged; Imager explicitly retained its active source and resumed ownership. All source holds were released.

Evidence remains under `target/runtime-proof/runtime-auto-attach/format-recovery/`, including backups, classification, exact restored-path lists and Git object/hash provenance. No concurrent semantic changes were discarded.
