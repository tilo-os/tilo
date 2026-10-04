# Automatic runtime attachment implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Execute tasks sequentially with independent review.

**Goal:** Ordinary independent cargo run launches of every framework GUI app automatically share the native runtime and MCP endpoint.
**Architecture:** Shared ui launch hook -> small runtime-portal bootstrap -> existing native runtime executable -> production Session and inherited SDK descriptors. A background daemon serves the same McpServer; stdio proxy clients attach independently.
**Tech Stack:** Rust, existing Unix transport/identity/broker, Tokio, Python proof orchestration.
**Spec:** docs/superpowers/specs/2026-09-17-runtime-auto-attach.md

## Global Constraints
- YAGNI, generic all-app path, existing authority and MCP semantics; platform code at edges.
- No per-app edits, aliases, package allowlist, alternate broker or automatic client installation.
- Preserve inherited Tilo connections and detached non-live modes. No UI global environment mutation.
- Preserve concurrent History/Photos launch.rs setter/tests and Browser work. Stage only owned hunks.

### Task 1: Shared bootstrap and background runtime
**Files:** runtime-portal new native bootstrap module/tests and narrow lib export; shell/runtime_host new daemon module/tests and narrow module export, unix connection refactor, tilo-runtime-mcp binary CLI extension; disjoint ui/src/launch.rs native-dispatch hook plus focused tests. Existing dependency manifests only if necessary; no new framework crate.
**Interfaces:** Public helper `runtime_portal::native::ensure_attached(app_id: &str) -> std::io::Result<()>`; when inherited complete connections exist it returns without environment/process changes, when no connections exist it transfers execution through native host, explicit opt-out returns standalone. Host modes `--daemon`, `--connect`, `--run-app APP_ID -- EXECUTABLE ARGS...`, optional `--profile`; preserve old `--profile PATH --apps MANIFEST`. Use one profile default and same override across modes. Final CLI/API adjustments may be ruled on for correctness, but no scope reduction.
- [ ] Add failing tests proving independent startup/reuse/current authority, inherited precedence, race/stale endpoint, argument preservation and failure behavior. For daemon tests use real Session/SDK seams with isolated subprocess environment.
- [ ] Implement smallest shared bootstrap and daemon. Missing source-checkout host must bootstrap automatically; startup readiness cannot be inferred from process existence. Refactor existing Unix connection preparation to serve spawn and exec without copying handshake rules.
- [ ] Integrate all native launch paths while keeping command/capture/headless/audit detached. Preserve current History additions. Test custom/default/definition gating with injected pure seam rather than starting real daemon inside ui unit tests.
- [ ] Run focused RED/GREEN, existing adapter/host regressions and build binary; retain logs, inspect exact owned diff, commit only owned changes. Write full report and expose precise commands/interface to proof task.

### Task 2: Independent native launch proof and delivery docs
**Files:** New scripts/runtime-auto-attach-proof.py and focused tests if orchestration has meaningful failure checks; docs/runtime-mcp.md and docs/runtime-auto-attach-todo.md. Retain native evidence under target/runtime-proof/runtime-auto-attach.
**Interfaces:** Consume Task1 automatic launch/helper and --connect; do not launch apps through manifest or manually bind their service descriptors.
- [ ] Write orchestration verifying daemon initially absent, independently launched apps register distinct peers via existing MCP, late joins are discoverable, actual action succeeds, one app exit does not end others/runtime/client. Include fourth ordinary app or equivalent real SDK generic-instance proof.
- [ ] Build actual current apps and invoke ordinary cargo run per app using short isolated TILO_RUNTIME_PROFILE, only selecting runtime binary for immutable provenance if required. Preserve natural app startup. Coordinate GUI before native run; do not bypass desktop security or user approvals.
- [ ] Record actual command lines, binary/source hashes, MCP transcript and terminal statuses, real app/runtime exact PIDs and final cleanup. Any missing native requirement remains open until proved.
- [ ] Update docs to explain zero-configuration app joining, daemon lifecycle, build bootstrap, persistent profile, inherited Tilo behavior, explicit assistant --connect setup and platform limitations. Report tests/evidence, self-review, commit owned files.

### Final gate
- [ ] Independent task reviews, whole-change review, at most one consolidated final fix wave and scoped review.
- [ ] Requirement-by-requirement completion audit, current executable smoke, todo and retained rulings/evidence, final user delivery.
