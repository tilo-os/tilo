# Framework History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox syntax for tracking.

**Goal:** Provide automatic shared History UI and isolated historical rendering across all managed persistent apps.

**Architecture:** A window-independent history controller projects the existing CRDT graph. The shared AppComponent host presents one surface and owns a separate preview renderer; authorized restoration enters the canonical record action frame.

**Tech Stack:** Rust, Automerge, existing ui/Vello/Taffy widgets and qos_actions.

**Spec:** docs/superpowers/specs/2026-09-16-framework-history-design.md

## Global Constraints

- Preserve all existing record history and unrelated concurrent work.
- Never replay external effects during preview or restore receipts/grants/runtime identity.
- No history preview may acquire live mutation, publication, persistence or invocation authority.
- Restore requires current authorization and exact reviewed record identity/frontier.
- Both embedded and standalone hosts and every managed persistent app are in scope.
- Isolated worktree already exists at /Users/xaver/dev/qos-macos-shell on codex/macos-shell. Do not create another or switch branches.
- Capture task-specific diffs before and after because the worktree has unrelated edits; do not stage another owner's changes.
- Run focused red/green tests and record exact evidence. Do not disturb the coordinated native proof source freeze.

### Task 1: Shared graph-based History controller

**Files:** Create ui/src/history_mode/mod.rs and ui/src/history_mode/tests.rs; add module export to ui/src/lib.rs only.

**Interfaces:** Consume existing HistorySource, HistoryEntryId, group_entries, compare_entries, AutoCommit and RecordId. Produce public HistoryController, HistoryResource, PreviewSide, HistorySnapshot, RestoreReview and HistoryError. Keep UI/host details out of these types. HistoryResource includes app id and record id (and namespace/room identity to prevent cross-workspace reuse); HistorySnapshot exposes immutable bytes and exact heads. RestoreReview binds resource, selection and reviewed live heads. Methods cover new, refresh, select, compare-side selection, snapshot, back_to_live, review_restore and validate_restore. The latter only validates; actual writes and permissions belong to Task 2.

- [ ] Write failing real-Automerge graph tests: create `title=one`, commit, update to `two`, commit; select first authored state; loaded snapshot reads `one`, live reads `two`, live heads unchanged. Add concurrent branch and verify a selected change's causal frontier excludes unrelated concurrent changes.
- [ ] Run `cargo test -p ui --lib history_mode` and record expected RED.
- [ ] Implement controller using existing history projection/comparison. Clone/fork for history reads so no implicit commit reaches the live record. Cache by heads. Use complete causal heads for a change or group, never just timestamps or positional indices as identity. Expose factual entries/groups and selected version metadata.
- [ ] Test comparison reads exact before/after; remote edit followed by refresh preserves preview bytes and flags live updates; back_to_live removes preview. Invalid selections return errors without dropping the existing valid preview.
- [ ] Implement review tokens and tests: wrong resource/workspace, no preview, stale live heads, and effect-only activity cannot validate restoration. Review invalidates on selection change. Validate without modifying document. Host will separately recheck authority.
- [ ] Run focused tests, inspect diff, save exact report and task diff. Commit only new files and the isolated export if safe; otherwise report the diff for controller-owned selective commit.

### Task 2: Shared host surface and enforced preview/restore boundary

**Files:** ui/src/application.rs, ui/src/widget/app_widget.rs, new focused ui/src/history_mode host/surface/preview modules, and narrow bound_dispatch integration if necessary. Add focused integration tests beside owning modules.

**Interfaces:** Consume Task 1's controller. Introduce a required explicit preview-rendering contract for registered persistent apps; exact signature is finalized against migration inventory before this task dispatch. The shared host owns the History surface and isolated preview lifetime. No application duplicates controller state. Use current CallbackBinding and owned ActionFrame for restore.

- [ ] Record precise host/preview interface and file ownership in the task brief before implementation, based on current inventory and peer feedback.
- [ ] Add failing production-path tests for automatic entry point, history selection, mutation/effect refusal, live-update isolation, Back to live, stale confirmation and current authorization.
- [ ] Implement host-owned button, grouped activity panel, selection/banner, before/after switching and reviewed Restore controls. Include narrow-width page switch, keyboard routing and accessibility.
- [ ] Keep live root untouched while constructing/operating a separate historical renderer; ensure preview descendants cannot escape into ordinary dispatch or native effects. Route permitted navigation through a restricted interface.
- [ ] Restore only app content through checked canonical owned action frame; preserve runtime metadata, report unsupported patch cases truthfully, refresh stale review and recheck authority at execution.
- [ ] Verify lifecycle, pending work, nested components and local view persistence; run relevant existing bound dispatch and application tests plus new regressions. Save report/diff for independent review.

### Task 3: Complete application migration

**Files:** all managed application registration/root files identified by /tmp/qos-history-migration-inventory.md; existing Writer history_panel/widget and File Explorer history integration; docs explaining the enforced contract.

**Interfaces:** Consume Task 2's finalized preview contract, automatic shared History surface, and host navigation intent. App renderers consume supplied snapshots without live services. Update the task brief with the authoritative complete app checklist before dispatch.

- [ ] Add conformance tests for every registered managed persistent application and native preview tests for at least text, photo and project schemas.
- [ ] Implement domain previews using normal app rendering and migrate app-owned History buttons/panels to the shared host. Do not treat a generic JSON/text dump as a native domain preview.
- [ ] Keep external/non-versioned content truthful (browser page/process effects); show recorded managed state/activity without reissuing effects. Check preview resource labels and scope for multi-resource apps.
- [ ] Verify existing saved records and view restoration remain readable, plus restored content, undo, and reopened persistence for representative schemas. Save coverage checklist and test evidence.

### Task 4: Cross-host interaction proof and final review

**Files:** production-path UI/headless integration tests, visual evidence under task workspace, and docs/shared history usage/migration guide.

**Interfaces:** Exercise final shared surface through production app construction in embedded and standalone paths.

- [ ] Test a real edit sequence, open History, select prior change, compare before/after, receive concurrent live change, reject stale restore, refresh review, restore, undo, close/reopen.
- [ ] Test permission revocation, workspace/resource switching and attempted keyboard/palette/effect dispatch while previewing.
- [ ] Render wide/narrow History layouts for text and Photos/project content and inspect actual images. Correct clipping/focus/labels before claiming completion.
- [ ] Run required focused and integration suites on supported host paths; investigate failures against concurrent baseline evidence.
- [ ] Obtain independent whole-feature review against the complete spec and migration inventory; address findings and audit every requirement before completion.
