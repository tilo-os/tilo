# Framework History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox syntax for tracking.

**Goal:** Provide automatic shared History UI and isolated historical rendering across all managed persistent apps.

**Architecture:** A window-independent history controller projects the existing CRDT graph. The shared App, FrameHarness and TileHost presentation owner presents one surface and owns a separate preview renderer; authorized restoration enters the canonical record action frame.

**Tech Stack:** Rust, Automerge, existing ui/Vello/Taffy widgets and tilo_actions.

**Spec:** docs/superpowers/specs/2026-09-16-framework-history-design.md

## Global Constraints

- Preserve all existing record history and unrelated concurrent work.
- Never replay external effects during preview or restore receipts/grants/runtime identity.
- No history preview may acquire live mutation, publication, persistence or invocation authority.
- Restore requires current authorization and exact reviewed record identity/frontier.
- Both embedded and standalone hosts and every managed persistent app are in scope.
- Run focused red/green tests and record exact evidence. Do not disturb the coordinated native proof source freeze.

### Task 1: Shared graph-based History controller

**Status:** Reviewed and committed in `3ba856d2e`; 10 controller tests pass.

**Files:** Create ui/src/history_mode/mod.rs and ui/src/history_mode/tests.rs; add module export to ui/src/lib.rs only.

**Interfaces:** Consume existing HistorySource, HistoryEntryId, group_entries, compare_entries, AutoCommit and RecordId. Produce public HistoryController, HistoryResource, PreviewSide, HistorySnapshot, RestoreReview and HistoryError. Keep UI/host details out of these types. HistoryResource includes app id and record id (and namespace/room identity to prevent cross-workspace reuse); HistorySnapshot exposes immutable bytes and exact heads. RestoreReview binds resource, selection and reviewed live heads. Methods cover new, refresh, select, compare-side selection, snapshot, back_to_live, review_restore and validate_restore. The latter only validates; actual writes and permissions belong to Task 3.

- [ ] Write failing real-Automerge graph tests: create `title=one`, commit, update to `two`, commit; select first authored state; loaded snapshot reads `one`, live reads `two`, live heads unchanged. Add concurrent branch and verify a selected change's causal frontier excludes unrelated concurrent changes.
- [ ] Run `cargo test -p ui --lib history_mode` and record expected RED.
- [ ] Implement controller using existing history projection/comparison. Clone/fork for history reads so no implicit commit reaches the live record. Cache by heads. Use complete causal heads for a change or group, never just timestamps or positional indices as identity. Expose factual entries/groups and selected version metadata.
- [ ] Test comparison reads exact before/after; remote edit followed by refresh preserves preview bytes and flags live updates; back_to_live removes preview. Invalid selections return errors without dropping the existing valid preview.
- [ ] Implement review tokens and tests: wrong resource/workspace, no preview, stale live heads, and effect-only activity cannot validate restoration. Review invalidates on selection change. Validate without modifying document. Host will separately recheck authority.
- [ ] Run focused tests, inspect diff, save exact report and task diff. Commit the new files and the isolated export.

### Task 2: Transactional content-only restoration engine

**Status:** Reviewed and committed in `6237abd52`; exact-root engine extended by Task2b.

**Files:** new ui/src/history_mode/restore.rs and focused tests; reuse existing Automerge APIs without changing the old undo helper for unrelated callers.

**Interfaces:** Consume Task 1's immutable snapshot/review and explicit app-owned content root schema. Produce a fallible restore preparation/application interface that does not commit or claim authorization. The host in Task 3 supplies exclusive canonical record action custody and current permissions. It must reject pending edits, unknown schema roots or unsupported patch semantics before touching live state.

- [ ] Add failing real document tests for text/list/map restoration, nested object resurrection, mark changes, preserved record metadata and receipts, and full rollback on failure. Root schema is an explicit app allowlist, never all keys except a guessed reserved prefix.
- [ ] Implement checked content patch application with every Automerge error propagated. Existing write_revert_patches is not safe to call: it discards write errors and ignores Mark/Conflict. Implement supported types and fail clearly on unsupported cases without partial mutation.
- [ ] Derive from the exact reviewed current heads to the selected full target heads. Verify content including types, list order, text and marks matches target and all non-content roots remain semantically unchanged. Apply against an isolated fork first; publish only verified changes through the later owned frame. No graph truncation, direct file write or fresh autosave owner.
- [ ] Test wrong/stale resource review, current pending transaction refusal, and no-op restoration. Leave one pending transaction for host commit with Restore version provenance; ensure failed/no-op restore does not add a synthetic action.
- [ ] Record focused RED/GREEN evidence and an independent task review. App allowlist declarations and live permission checks remain required in later tasks; engine tests alone cannot establish authorized restoration.

### Task 2b: Explicit mixed-content schema selection

**Status:** Reviewed and committed in `4d6bdf425`; 36 restore tests and 46 full History tests pass.

**Discovery:** The complete app audit found dynamic root families (Browser bookmarks, Store drafts) and mixed nested trees (Sheets workbook content/view, Remote profile/presence). Whole-root restoration alone cannot satisfy the all-app contract.

- [ ] Extend the checked engine with explicit audited root-family and nested content selectors. Resolve current/target key membership without guessing all roots or leaking generated static schemas. Reject ambiguous/overlapping/reserved selectors before mutation.
- [ ] Preserve current operational fields and view state in mixed trees; define keyed deletion/resurrection behavior explicitly and test it using real app schema shapes. Never match list members by a mutable positional index when preserving per-item state.
- [ ] Keep the same exact-frontier, typed-content, rollback, no-op and one-pending-transaction guarantees. Add separate focused RED/GREEN evidence and independent review before enabling mixed-root app restore.

### Task 3a: Canonical owned restore transaction

**Status:** Reviewed and committed in `022e935ed`; 11 focused and 59 native tests pass. Existing bound admission mismatch is a required Task3b correction.

**Files:** narrow ui/src/state and bound_dispatch changes; focused history transaction tests.

- [ ] Share a narrowly typed exclusive document/UndoLog guard and canonical stamp/span finalizer between existing legacy State and bound ActionFrame. Do not construct a temporary RecordStore, replace live documents, or fabricate a permissive callback binding.
- [ ] Reserve undo/generation capacity before writing. Reject pending physical edits and unresolved logical undo baselines without committing them. Use the exact reviewed current frontier as the undo span start.
- [ ] Apply the prepared engine without commit, then stamp exactly once with current authored context and add exactly one undo entry. No-op and failure preserve redo/history revision.
- [ ] Prove legacy and bound undo, baseline/pending rejection, rollback, no-op and no duplicate action bookkeeping; review independently. Current authority and lifecycle admission remain mandatory in Task3b.

### Task 3b: Shared host surface and enforced preview/restore boundary

**Files:** ui/src/app.rs, ui/src/headless.rs, ui/src/widget/tile_host.rs, ui/src/application.rs, ui/src/widget/app_widget.rs, ui/src/launch.rs, focused ui/src/history_mode host/surface/preview modules, and narrow permissions/binding-policy integration where required. Add focused integration tests beside owning modules.

**Interfaces:** Consume Task1's controller, Task2/2b's checked restore engine, and Task3a's canonical owned restore transaction. Introduce a required explicit preview-rendering contract for registered persistent apps; exact signature is finalized against migration inventory before this task dispatch. The shared host owns the History surface and isolated preview lifetime. No application duplicates controller state. Use current CallbackBinding and owned ActionFrame for restore.

- [ ] Record precise host/preview interface and file ownership in the task brief before implementation, based on current inventory and peer feedback.
- [ ] Add failing production-path tests for automatic entry point, history selection, mutation/effect refusal, live-update isolation, Back to live, stale confirmation and current authorization.
- [ ] Implement host-owned button, grouped activity panel, selection/banner, before/after switching and reviewed Restore controls. Include narrow-width page switch, keyboard routing and accessibility.
- [ ] Keep live root untouched while constructing/operating a separate historical renderer; ensure preview descendants cannot escape into ordinary dispatch or native effects. Route permitted navigation through a restricted interface.
- [ ] Restore only app content through checked canonical owned action frame; preserve runtime metadata, report unsupported patch cases truthfully, refresh stale review and recheck authority at execution.
- [ ] Verify lifecycle, pending work, nested components and local view persistence; run relevant existing bound dispatch and application tests plus new regressions. Save report/diff for independent review.

#### Task3b1 review checkpoint: shared presentation and managed headless host

**Status:** Reviewed and committed in `feb28d510`; independent review fixes accepted. History: 65, headless: 50, accessibility: 31 passing tests; unchanged TileHost: 22, bound: 25, timeline: 48 passing tests. Production/native integration remains Task3b2.

Complete the reusable History surface/preview/registration contract and real managed FrameHarness integration, corrected wide/narrow layout, pointer/keyboard/accessibility selection, pinned comparison, return-to-live, dirty-editor focus/pending preservation, restricted preview navigation/local dispatch isolation, and strict existing-record admission regressions. Review and commit this coherent checkpoint independently. It is not completion of Task3b or any app migration; restoration must not expose an operative control without authorized admission.

#### Task3b2: production hosts, authorized restoration and provider lifecycle

After Task3b1 review, integrate the same presentation into native App and TileHost with exact owners, current authority/expiry and Task3a restore custody; implement source/asset response generation checks and managed launcher/component bundles. Finish restore review/cancel/stale refresh, current permission/lifecycle tests, native/tiled focus/input isolation, publication/persistence scheduling and cross-host regressions. All original Task3b requirements above remain binding; the split changes only implementation/review boundaries.

### Task 4a: Checked filesystem restore backend

**Discovery:** Files restore currently appends repository/history state before its namespace comparison, and its request carries only a selected version. Connecting that path to a reviewed dialog would not satisfy stale-review rejection or undo/durability.

- [ ] Add a bounded checked restore protocol backed by the existing filesystem record/history/publisher/transaction owners. The backend issues and validates an exact reviewed resource/frontier precondition; the shared host never falls back to unchecked restore.
- [ ] Validate current authority, resource binding, supported codec, pending writes and the full reviewed frontier before durable admission. Reuse existing journaled transaction machinery; reject stale/denied requests without authored mutation and distinguish failures after durable admission as recovery-pending outcomes.
- [ ] Preserve exact result identity and new restore provenance, make unchanged content a true no-op, provide backend-owned undo/redo, and prove durable reopen/idempotent retry. Never undo the incidental Files app State.
- [ ] Refuse plain-byte rewind for both legacy and signed codec-backed documents; route supported restoration to the actual document owner with a fresh review, or show the precise unsupported mapping.
- [ ] Add real race/late-revocation/ABA/concurrent-head/fault-recovery tests, then independent review. Audit source and protocol details in the task-specific brief before implementation.

### Task 4b: Complete application migration

**Files:** all managed application registration/root files identified by a migration inventory of every managed app; existing Writer history_panel/widget and File Explorer history integration; docs explaining the enforced contract.

**Interfaces:** Consume Task 3's finalized preview contract, automatic shared History surface, and host navigation intent. App renderers consume supplied snapshots without live services. Update the task brief with the authoritative complete app checklist before dispatch.

- [ ] Add conformance tests for every registered managed persistent application and native preview tests for at least text, photo and project schemas.
- [ ] Implement domain previews using normal app rendering and migrate app-owned History buttons/panels to the shared host. Do not treat a generic JSON/text dump as a native domain preview.
- [ ] Keep external/non-versioned content truthful (browser page/process effects); show recorded managed state/activity without reissuing effects. Check preview resource labels and scope for multi-resource apps.
- [ ] Verify existing saved records and view restoration remain readable, plus restored content, undo, and reopened persistence for representative schemas. Save coverage checklist and test evidence.

### Task 5: Cross-host interaction proof and final review

**Files:** production-path UI/headless integration tests, visual evidence under task workspace, and docs/shared history usage/migration guide.

**Interfaces:** Exercise final shared surface through production app construction in embedded and standalone paths.

- [ ] Test a real edit sequence, open History, select prior change, compare before/after, receive concurrent live change, reject stale restore, refresh review, restore, undo, close/reopen.
- [ ] Test permission revocation, workspace/resource switching and attempted keyboard/palette/effect dispatch while previewing.
- [ ] Render wide/narrow History layouts for text and Photos/project content and inspect actual images. Correct clipping/focus/labels before claiming completion.
- [ ] Run required focused and integration suites on supported host paths; investigate failures against concurrent baseline evidence.
- [ ] Obtain independent whole-feature review against the complete spec and migration inventory; address findings and audit every requirement before completion.
