# Real Notes Component and Legacy Import Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task under the controller's single-source-author protocol. Steps use checkbox (`- [ ]`) syntax. This is a bounded candidate for the already-authorized native Mac shell goal, not an implementation or a new approval request. The planning worker changes only this ignored candidate; the active direct shell cutover author retains sole production ownership.

**Goal:** Run the existing multi-page Notes application from one canonical definition and record, with explicit retry-safe legacy import, real domain actions and independent recoverable instance Views.

**Architecture:** Reuse `NotesApp`, `NotesDoc`, `MainPanel`/`BlockEditor`, sidebar, search, status and Notes resource/action implementations. The common framework owns the document, writer, authenticated action context and lifetime; Notes projects that document into its existing widgets without workspace or Markdown savers. Import publishes the entire notebook and its durable receipt in the unique new-record initialization boundary; no import occurs on ordinary construction, sync or restoration.

**Tech Stack:** Existing Rust workspace, qos-app (imported as `ui` by Notes), Automerge, qos-actions, serde/serde_json, existing atomic-file and app-state storage, workspace blake3 for deterministic import keys, nextest and the established isolated runner. No shell dependency, new widget framework or alternate Notes UI.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md` §§4–8, 11; Notes row of `docs/superpowers/specs/2026-09-12-macos-app-capability-map.md`; `.superpowers/sdd/2026-09-12-macos-shell-build-foundation/real-app-host-contract-design.md`; prerequisite `.superpowers/sdd/2026-09-12-macos-shell-build-foundation/real-app-framework-plan-draft.md` (unique RecordReservation, mutable HostContext initializer, retained ComponentSetup error, sticky writer/full-checkpoint recovery revision).

## Global Constraints

- “The first milestone is **real Notes and Files, with a complete capability map for every other app**.” This candidate implements the Notes component dependency only.
- “Apps and `ui` must not depend on `shell` to request a service.”
- “Two tiles opening one record share its document, but keep independent Views and operation contexts.”
- “A construction or restoration failure leaves a retryable error tile and retains the original persisted data.” The Notes slice returns retained errors; shell error tiles are a later consumer.
- “Headless execution remains a first-class runner of this same host contract.”
- “Benchmarks and tests must not load the user's real state or secrets.”
- “Keep Linux defaults and HDR behavior.”
- “Initial target: Apple Silicon. macOS 13 is a proposed deployment minimum to validate against the pinned SDK/toolchain and physical machines before release.”
- One production source author at a time, with controller-assigned ownership after direct core cutover and common framework acceptance. No source edits/builds/VM/GUI work are authorized to this planning worker.
- Do not start collaboration transport, enroll identity, add path-bearing grants, implement Files/catalogue/native windows, or claim the milestone complete in this slice. These remain concrete later consumers of this component.

## Inspected source and resulting boundaries

| Source | Finding and decision |
| --- | --- |
| `apps/notes/src/storage.rs:75–214` | `workspace.automerge` is a fresh Automerge document containing Text `workspace_json` = JSON `WorkspaceData`; it is not the live Notes schema. Page text is separately `pages/<uuid>.md`. `read_page_body` turns missing files into empty text and callers commonly swallow other errors. Explicit import must use strict reads and validate the complete snapshot first. |
| `apps/notes/src/storage.rs:224–605` | Saver/PageSaver own background threads and coalesced independent writes. `flush()` only sends a message; PageSaver's alleged blocking helper does not acknowledge I/O. They cannot be used as canonical durability evidence. Preserve legacy codec/standalone file-I/O tests, remove these workers from all production component construction. |
| `apps/notes/src/app.rs:239–343,646–887` | `spawn_inner` creates layout, loads metadata/Markdown, then starts both savers. First-sync grace decides seed/adopt; empty lists re-seed from Markdown. A host constructor must be extracted before any of those effects; canonical projection must accept an empty notebook and empty page without seeding. |
| `apps/notes/src/app.rs:1040–1169,1224–1262` | Select/create/delete/duplicate/open jump use Markdown files; pending snapshots can race page selection. Replace normal-event paths with canonical document reads and the existing typed action route, including draining editor deltas before switching. |
| `apps/notes/src/main_panel.rs:284–321,404–452` | `bind_page` parses Markdown even when adopting CRDT blocks; `seed_block_ops` drops inline marks while inserting blocks. Import must compile block operations AND native marks once, resolving page links against the complete page set. Canonical bind never parses disk text. |
| `apps/notes/src/doc.rs:224–245,317–535` | Real schema is root `pages`, `tags`, `tag_assignments`, per-page Text title/parent/order/List blocks, block IDs/kinds/Text/native marks. Page creation already creates its empty block list. `notes_marks_version` records explicit formatting migration per block. Reuse this shape; add only notebook schema/import metadata. |
| `apps/notes/src/model.rs:60–82` | `selected` is in legacy WorkspaceData, while pages/tags/assignments are shared domain data. Selection remains local after import. `check_invariants` lacks duplicate-page/cycle checking; strict import adds those checks without silently renumbering malformed input. |
| `apps/notes/src/main.rs:72–112` | Production uses `spawn_default`, process OnceLock peer ID, empty State and disabled framework autosave; native configure_app alone registers actions. Replace with `LaunchSpec::from_definition` and framework host preparation; authentication comes from HostContext, not actor rewriting. |
| `apps/notes/src/app.rs:1720–1840`, `actions.rs:49–114` | Preserve existing resource IDs (`notes.page:…`, `notes.block:…`, `notes.tag:…`), local OpenPage/OpenTag operations, search and presence contribution. The framework supplies instance routing and authority separately. |
| `ui/src/view.rs:31–88`, `widget/mod.rs:1029–1038` | View has scroll/focused-child state/open resource/viewport/view-mode only. It cannot faithfully carry selected page plus drawer/filter and stable editor anchors. Add a typed app-local contribution to the local instance envelope, without changing the existing FrozenApp wire. |
| `apps/notes/Cargo.toml`, `tests/it.rs` | `autotests=false`; every new integration module must be explicitly registered in `it.rs`. Existing file, Markdown, event, two-peer, scroll and render tests remain meaningful regression coverage, but must not masquerade as tests of the canonical factory. |

## Prerequisites and exact common interfaces

Execute only after the common framework's five tasks have landed and passed native/exact-SHA Fedora gates. Its declarations remain authoritative; do not independently recreate them in Notes:

```rust
// Consumed exactly, through qos-app's ui reexports.
type ComponentFactory = fn(HostContext, LaunchTarget, RestoreInput)
    -> Result<AppComponent, LaunchError>;
HostContext::prepare(&ApplicationDefinition, RecordStore, Rc<InstanceViewStore>, String, u64,
    Rc<RefCell<PermissionContext>>, UserId, ActionContext, &LaunchTarget)
    -> Result<HostContext, LaunchError>;
HostContext::record(&self) -> &RecordCheckout;
HostContext::author(&self) -> &UserId;
HostContext::read_record<R>(&self, impl FnOnce(&AutoCommit) -> R)
    -> Result<R, RecordOpenError>;
HostContext::initialize_new_record(&mut self,
    impl FnOnce(&mut AutoCommit) -> Result<(), String>) -> Result<(), RecordOpenError>;
ApplicationDefinition::instantiate(&self, HostContext, LaunchTarget, RestoreInput)
    -> Result<AppComponent, LaunchError>;
AppComponent::new(HostContext, Box<dyn AppWidget>, ActionRegistry) -> AppComponent;
RecordStore::reserve_record(&self, &RecordCheckout) -> Result<RecordReservation, RecordOpenError>;
RecordReservation::initialize(&mut self,
    impl FnOnce(&mut AutoCommit) -> Result<(), String>) -> Result<(), RecordOpenError>;
RecordStore::try_open_record(&self, &RecordCheckout) -> Result<RecordOpenOutcome, RecordOpenError>;
RecordStore::try_flush(&self) -> Result<(), String>;
RecordStore::save_status(&self, &RecordId) -> Result<RecordSaveStatus, RecordOpenError>;
RecordStore::durability_error(&self, &RecordId) -> Option<String>;
```

New initialization consumes the unique reservation owned by mutable HostContext. Notes does not initialize by shared store+ID or finalize/rebase during mount. Existing target only reads/projects. Import uses its own unique reservation before constructing the same factory with `LaunchTarget::Record { checkout, reveal: None }`. Failed initialized construction is retained by `LaunchError::ComponentSetup { source, record, records }`; preserve this error rather than flattening it to String. Import durability errors likewise retain a strong store and exact checkout. The store's failed-writer state is sticky; retry is owner `try_flush()` with a full checkpoint, not a Notes-specific append retry.

`RecordStore::try_flush()` drives and awaits every open writer in the shared package/workspace owner and may return aggregate failures for other records. Import therefore classifies only its destination after that owner-wide call with `save_status(&checkout.id)`. A destination `Saved` is sufficient even if the owner-wide call also reported an unrelated record. Destination `Failed(message)`, `Saving`, `MemoryOnly`, or a destination-specific `RecordOpenError` maps to `ImportError::Durability` with an explicit message, the exact checkout and the strong `RecordStore`; early Existing/open errors retain `ImportError::Record`.

## File responsibilities

- New `apps/notes/src/component.rs`: singleton definition, factory, real AppWidget metadata; no ambient startup or import.
- New `apps/notes/src/notebook.rs`: fallible version-1 notebook validation/initialization and read-only projection built on NotesDoc; no writer/thread ownership.
- New `apps/notes/src/legacy_import.rs`: strict snapshot, deterministic source/destination mapping, reserved initialization, receipt and retained import errors.
- New `apps/notes/src/local_view.rs`: NotesLocalViewV1 encoding/validation and current-document target resolution.
- `apps/notes/src/{app,main_panel,markdown,doc,actions,model}.rs`: extract shared widget construction; canonical binding, page/action behavior, rich snapshot compilation and duplication.
- `apps/notes/src/{main,lib,shots}.rs`, `Cargo.toml`, `README.md`: shared entrypoint, exports/dependencies, explicit fixture preparation, migration documentation.
- `ui/src/widget/app_widget.rs`, `ui/src/{application,application_view}.rs`: minimal app-local View envelope extension and bound hooks; `ui/src/view.rs`'s View/FrozenApp wire stays byte-compatible.
- Existing `ui/src/widget/block_editor.rs` selection APIs are consumed unchanged: `document_selection`, `focused_index`, `focus_block`, `select_document_range`, and TextInput cursor/selection accessors suffice.
- New `apps/notes/tests/{canonical_component,canonical_import,canonical_actions,canonical_views}.rs`, `tests/support/canonical.rs`, module registrations in `tests/it.rs`; new envelope compatibility tests in existing `ui/tests/application_contract.rs`.

## Canonical schema and import protocol

Notebook schema V1 keeps the existing NotesDoc maps and block representation. Add root `notes_schema_version = 1` and optional Map `notes_import_v1` with `source_key` (64 lowercase hex chars), `snapshot_digest` (64 hex chars), `destination_id` (RecordId string), `format = "workspace-json-markdown-v1"`, `version = 1`. This metadata is outside page/tag operations and cannot disappear when all pages/tags are deleted. Do not put raw source paths, local selected page, document bytes, credentials or device-specific UI state in this shared receipt. The record service stamps package/workspace/home/record identity; Notes never overwrites those fields.

A new ordinary notebook starts with these three empty Maps and schema version only; no Welcome page. Existing canonical documents require version 1 and correct field/object shapes, valid IDs/tree/tag references and valid block structure. Missing/unknown version is a retained migration-required error, never a reason to reset or silently stamp the live document. Legacy rich `FrozenApp.state` is not this file import format: `RestoreInput::LegacyRecovery` remains the common framework's retained `RecoveryRequired`, with no automatic adoption or doc copy. Support for another explicit recovery source can be a separate bounded migration later.

Define these Notes-owned APIs:

```rust
pub const NOTES_SCHEMA_VERSION: u32 = 1;
pub fn definition() -> &'static ui::application::ApplicationDefinition;
pub fn create_component(mut context: HostContext, target: LaunchTarget, restore: RestoreInput)
    -> Result<AppComponent, LaunchError>;
// notebook.rs
pub fn initialize_empty(doc: &mut AutoCommit) -> Result<(), String>;
pub fn validate(doc: &AutoCommit) -> Result<(), String>;
pub fn project(doc: &AutoCommit, selected: Option<PageId>) -> Result<Workspace, String>;
// legacy_import.rs; fields private except the returned result.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LegacySourceKey([u8; 32]);
impl LegacySourceKey { pub fn from_bytes(bytes: [u8; 32]) -> Self; }
pub struct LegacySnapshot { workspace: Workspace, pages: Vec<ImportedPage>, digest: [u8; 32] }
pub struct ImportedPage { id: PageId, ops: Vec<ui::widget::BlockEditOp>,
    marks: Vec<(String, Vec<crate::actions::InlineMarkSpan>)> }
pub struct ImportOutcome { pub checkout: RecordCheckout, pub already_imported: bool,
    pub source_key: String, // stable lowercase hex for explicit CLI retry
    pub initial_selected: Option<PageId>, pub warnings: Vec<String> }
pub enum ImportError {
    Source(String), Conflict(RecordCheckout), Record(RecordOpenError),
    Durability { message: String, source_key: LegacySourceKey, checkout: RecordCheckout, records: RecordStore },
}
pub fn source_key(root: &std::path::Path) -> Result<LegacySourceKey, ImportError>;
pub fn destination(key: LegacySourceKey) -> RecordCheckout;
pub fn read_snapshot(root: &std::path::Path, key: LegacySourceKey)
    -> Result<LegacySnapshot, ImportError>;
pub fn import_legacy(records: &RecordStore, root: &std::path::Path,
    key: LegacySourceKey) -> Result<ImportOutcome, ImportError>;
```

`source_key` is a local CLI migration convenience: BLAKE3 of `b"qos-notes-legacy-source-v1\0"` plus the canonicalized explicit source directory's native OS path bytes. It performs no write or ambient HOME lookup. Key stability applies to that source location/profile; moving/copying it is a distinct explicit import. An external provider may later supply a stable opaque source identity and snapshot; this local function is not a filesystem grant API. Tests supply fixed keys. `destination` uses existing `RecordId::derived("notes-legacy-v1", &RecordId::parse(hex_key).expect("nonempty hex"))`, with `RecordKind::new("notes.notebook")`; production avoids expect by checked construction. Workspace/package scoping stays in RecordStore. Source content changes do not change destination. Receipt validates the exact destination and source key. A deliberate second import of changed/copied source requires an explicit fresh source identity/new destination operation; do not choose that conflict escape automatically.

Import order:

1. Compute the deterministic destination and try Existing. If loaded/AlreadyOpen, validate notebook+receipt. A matching receipt means no domain mutation and no source reads, even after deliberate page deletion or source removal/change. Drive acknowledgement with owner-wide `records.try_flush()`, then classify this destination with `records.save_status(&checkout.id)`: `Saved` permits success even if another open record caused the aggregate flush call to fail; `Failed(message)`, `Saving`, `MemoryOnly`, or a destination-specific `RecordOpenError` returns `ImportError::Durability` with an explicit message, checkout and owner and does not claim import success. A different/missing receipt is Conflict. Corrupt/unsupported/busy is its original error, not MissingRecord.
2. Only MissingRecord allows strict source read. Read metadata and every referenced page byte sequence before reserving; read all again and compare before initialization, rejecting a changed snapshot. Active old Notes must be quiescent: independent per-file savers provide no cross-file snapshot transaction. Do not claim a second read proves an uncooperative source cannot race. Retain source and report the condition; no deletion, renaming or layout creation in import.
3. Strict-load Automerge (partial-tail Error) and deserialize workspace_json. Reject missing/invalid files, invalid UTF-8, duplicate page/tag IDs, cycles, orphan parents, noncontiguous sibling order, bad tag edges and invalid selected page. Missing referenced Markdown is an error, including on old Phase-1 sources; an existing zero-byte Markdown file is a valid empty page. Error names the affected page/path locally. Unreferenced files remain untouched and outside the notebook snapshot.
4. Parse via existing `markdown::parse`, resolve links against ALL titles and stable page IDs. Duplicate titles with a referenced ambiguous `[[Title]]` produce an explicit source error rather than arbitrary HashMap choice. Unresolved links retain their visible label without creating a nil-target canonical link; report those labels in the import result/diagnostic, never silently turn them into valid links. Compile existing block kinds (heading, bullets/numbering+indent, checklist state, code language, divider), plain UTF-8 text and InlineMarkSpan through shared conversion. Use deterministic imported block IDs from source-key/page-ID/block-index with the existing BLAKE3 dependency; no fresh random UUID inside replay. Preserve original Markdown bytes in the retained source, not a second canonical document field.
5. Acquire unique `reserve_record`, initialize schema, all pages, tags, membership, rich block operations and native marks, then receipt, in its private candidate document. Use NotesDoc methods; propagate every fallible block/mark operation. This is initialization before history, not user ActionHost dispatch. Validate the completed notebook before returning Ok. Every block gets explicit canonical marks (including empty mark lists) so bind cannot re-seed formatting.
6. Drive the actual writer with owner-wide `records.try_flush()`, then inspect `records.save_status(&checkout.id)` for the imported destination. Return a successful outcome only for destination `Saved`; an aggregate flush error belonging solely to another open record does not make this import fail. Destination `Failed(message)` returns `ImportError::Durability` with the destination message, source key, exact checkout and strong store. Destination `Saving` or `MemoryOnly` likewise returns that retained variant with an explicit status message; a destination-specific `RecordOpenError` is represented by the same retained variant with its error text and owner/checkout context, rather than the ownerless `ImportError::Record` variant used for early Existing/open failure. Retry through Existing sees the same receipt and invokes the owner's sticky full-checkpoint recovery. The source remains intact. Crash before the first atomic publication leaves no canonical bytes; retry reconstructs the same destination. The framework may start that complete publication asynchronously after initialization, before the explicit flush call; content and receipt remain one atomic snapshot and successful enqueue never reports durability. Crash after publication leaves content+receipt together. Framework strict-open/recovery rules apply to damaged tails; a damaged destination is never reimported over.
7. Initial selected page is returned only for the first successful import and passed to a local instance View by the explicit caller. Retried import does not reset another instance's selection. Destination mapping is the stable pure function plus in-document receipt, so no independently published completion sidecar can falsely get ahead of content.

## Task 1: Strict notebook schema and all-or-nothing legacy import

**Files:** new `apps/notes/src/notebook.rs`, `apps/notes/src/legacy_import.rs`; modify `apps/notes/src/{doc,markdown,main_panel,model,lib}.rs`, `apps/notes/Cargo.toml`, and `ui/src/action_palette.rs`; adapt the existing regressions in `apps/notes/src/actions.rs`, `apps/notes/src/app.rs`, and `apps/notes/src/shortcuts_overlay.rs`; new `apps/notes/tests/canonical_import.rs`, `apps/notes/tests/support/canonical.rs`, registration in `apps/notes/tests/it.rs`.

**Consumes:** common unique RecordReservation and fallible storage APIs above; existing Markdown/NotesDoc code. **Produces:** exact notebook/import APIs above, strict complete snapshot with deterministic rich blocks and a durable receipt.

- [ ] Add fixed fixture builder in `tests/support/canonical.rs`: temp source and state directories, explicit `RecordOwners`, package `dev.qos.notes`, workspace `fixture-room`, fixed page/tag UUIDs from `Uuid::from_u128`, fixed LegacySourceKey and an explicitly authenticated fixture author. Source has root “Projects”, child “Launch”, sibling “Empty”, a tag and edge, selected child, and bodies containing every current block kind, Unicode, bold/italic/code/link spans. Write real legacy bytes using `storage::encode_workspace`; write the zero-byte Empty file explicitly. Keep shell/outer record distinct from Notes record. No HOME/environment changes, timers, personal source paths or ambient spawn constructors.

```rust
// canonical_import.rs: use the concrete fixture builder implemented above.
let fixture = LegacyFixture::rich();
let before = fixture.source_bytes();
let imported = import_legacy(&fixture.records, &fixture.source, fixture.key).unwrap();
fixture.records.try_flush().unwrap();
let snapshot = fixture.records.read(&imported.checkout.id, |doc| {
    (NotesDoc::list_pages(doc), NotesDoc::list_tags(doc),
     NotesDoc::page_block_marks(doc, fixture.child))
}).unwrap();
assert_eq!(snapshot.0.len(), 3);
assert_eq!(snapshot.1[0].id, fixture.tag);
assert!(!snapshot.2.is_empty());
assert_eq!(fixture.source_bytes(), before);
assert!(import_legacy(&fixture.records, &fixture.source, fixture.key).unwrap().already_imported);
```

`LegacyFixture::rich()`, `source_bytes()` and explicit store reopen helper are test-only methods in the support file; source_bytes returns a sorted relative-path→bytes map of all fixture files, including unreferenced file bytes. Add assertions for exact parent/order/IDs, each kind/text/mark/range/link target, tag membership, and selected page returned locally. Compare semantic content and stable IDs, not random Automerge actor/save bytes.

- [ ] Add failures for each malformed source condition; assert destination has no Ready document or newly published bytes and source map is unchanged. Include cyclic/duplicate pages because current Workspace invariant validation misses them. Add known-format unsupported data and ambiguous links. Re-read comparison failure is injected at the read boundary by a private test helper, not a public fake service.
- [ ] Add crash/publication tests: stop before reservation, fail initializer after first page, fail first flush, reopen after successful flush, open an unrecoverably corrupt destination, and open a destination with a recoverable partial tail. Failure after Ready retains checkout/store and succeeds on retry after repairing the test directory without any new edit. No import completion is reported by enqueue acknowledgement. Repeat after deleting every imported page/tag through domain actions; page count stays zero. Failures rejected before framework acceptance—unrecoverable corruption, framework schema refusal and identity refusal—leave the original bytes untouched. A complete, unrepaired destination without a matching receipt is likewise byte-identical after Conflict. For a valid-prefix partial tail, framework acceptance may atomically replace the canonical bytes with a complete encoding of the accepted prefix; Notes then validates the notebook using its existing error contract. Only after successful notebook validation does it reuse a matching receipt or return Conflict for a missing/mismatched receipt; it never reimports over that accepted destination. Two simultaneous attempts either use the one matching completed record or report Busy; no second writer/reservation initializer runs. Do not assert that accepted-prefix repair only removes the torn suffix: `doc.save()` may fully re-encode the prefix.
- [ ] Add a shared-owner two-record durability regression. With an unrelated record's writer failing, import and flush the deterministic destination and assert its `save_status` is `Saved` and import succeeds despite the aggregate owner-wide flush error. In the converse case, make the imported destination writer fail while the unrelated record is `Saved`; assert import returns the retained destination checkout/store and destination failure, then succeeds through Existing after repair without a new edit or source mutation.
- [ ] Close the three exercised ambient edges in existing regressions without removing or filtering assertions. In `app::tests::page_delete_records_one_undo_boundary_and_reverts_in_the_app`, replace `ui::collaboration::local_user_id()` with `ui::permissions::UserId("notes-app-test-author".into())`, pass that same value to `record_dispatched_action` and `undo_as`, and retain the recorded-author and restored-page assertions. In `shortcuts_overlay::tests::shared_modal_hit_testing_preserves_inside_and_dismiss_paths`, build `ui::font_catalog::FontCatalog::controlled_fixture(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/assets/fonts"), "en-US")` and a `TextRenderer::with_font_catalog(&catalog)` for the real paint helper; retain both inside/close and scrim dismissal paths. In `actions::tests::ctrl_k_publishes_the_nested_page_tree`, build the same controlled catalog/renderer and pass it to the new public owned-renderer palette constructor below; retain the nested resource and real palette-result assertions.

```rust
// ui/src/action_palette.rs
pub fn with_text_renderer(renderer: TextRenderer) -> Self {
    // Read the locale before moving the renderer into the palette.
    let language = renderer.font_system.locale().to_owned();
    Self::with_source(
        Some(renderer),
        PaletteFieldSource::Language(language),
    )
}
```

This narrow public constructor owns the controlled renderer required by normal palette layout and uses that renderer's locale for its SearchField. Keep `with_host_text` crate-private: exposing it alone would give external callers a palette with no owned layout renderer. Preserve `new`, `with_inputs`, the host construction route, and production defaults. Do not add a public `Block`/`RichTextLine` constructor solely to remove benign locale reads from the remaining widget and legacy-spawn assertions. Retain the existing temporary-directory starter/spawn tests and their joined saver workers; the new importer remains pure parsed-data construction and never invokes those constructors.
- [ ] Run the Task 1 focused test command below and verify intended failure; implement initialization, strict snapshot compiler and receipt protocol. Extract shared kind/mark conversion from MainPanel rather than copy parser semantics. Use explicit result propagation and validate before committing private initialization. Do not change legacy read_page_body semantics relied on by file-I/O tests; the new importer deliberately uses strict reads.
- [ ] Run Task 1 tests plus existing Markdown/page file/workspace model tests and mandatory native compile/lint/format gates. Self-review then commit `feat(notes): import legacy notebooks into canonical records`; controller reviews that commit and runs the same selections on Fedora at its exact SHA before acceptance.

## Task 2: One real factory and canonical page/action lifecycle

**Files:** new `component.rs`; `app.rs`, `main_panel.rs`, `actions.rs`, `doc.rs`, `lib.rs`; `ui/src/{record,application}.rs`, `ui/src/widget/app_widget.rs` for the save-status seam; new `canonical_component.rs`, `canonical_actions.rs`, registrations/support fixtures.

**Consumes:** Task 1 schema and common factory/bound action routing. **Produces:** `definition()` and `create_component` above; `NotesApp::from_document(doc: &AutoCommit, author: &UserId) -> Result<NotesApp, String>` and `MainPanel::bind_canonical_page(doc: &mut AutoCommit, page: PageId, title: &str, blocks: ObjId) -> Result<(), String>` (projection-only despite the editor's mutable binding signature).

- [ ] Write factory tests first: New produces empty schema-ready notebook; Existing rich fixture projects real sidebar/editor resources; factory registration permits the existing notes.action through App and Driver. Mount two instances on the same record with the same author and distinct instance IDs. Capture heads before construction/repeated sync/binding/selection and require no schema, marks or empty-block writes. Reopen an emptied imported notebook with unchanged legacy files still present and prove nothing resurrects. Creating two New targets yields distinct records.

```rust
// component.rs construction shape; common instantiate owns restore validation.
fn create_component(mut context: HostContext, target: LaunchTarget, _restore: RestoreInput)
    -> Result<AppComponent, LaunchError>
{
    if matches!(target, LaunchTarget::New) {
        context.initialize_new_record(crate::notebook::initialize_empty)
            .map_err(LaunchError::Record)?;
    }
    let author = context.author().clone();
    let root = context.read_record(|doc| NotesApp::from_document(doc, &author))
        .map_err(LaunchError::Record)?.map_err(LaunchError::Operation)?;
    let mut registry = qos_actions::ActionRegistry::new();
    crate::actions::register(&mut registry);
    Ok(AppComponent::new(context, Box::new(root), registry))
}
```

The singleton is `static DEFINITION: std::sync::LazyLock<ApplicationDefinition>` (its concrete common fields are immutable strings/value metadata, vectors of PermissionScope and a factory function pointer); `definition()` returns `&DEFINITION`. It must expose exactly one immutable definition value (not an app singleton), ID `dev.qos.notes`, title Notes, `RecordKind::new("notes.notebook")`, `IconDescriptor::Named("notes".into())`, and real factory. Empty required/optional scope lists mean this slice consumes record/authority services already supplied by the framework; do not advertise unsupported spellcheck/collaboration service availability or conflate absence with denial.

- [ ] Implement AppWidget directly on NotesApp using existing `AppKind::Editor` as the legacy presentation hint, title/icon and `permission_hint() -> None`. AppKind currently has only Terminal/Files/Editor/Browser/Settings; do not invent an existing Notes variant or couple package identity to that enum. Canonical package authority is `dev.qos.notes` through the bound host context, independent of the legacy hint. Preserve existing Widget resource/operation/search/presence/accessibility/action methods; do not add duplicate AppCommand callbacks bypassing NotesAction. Extract a single shared in-memory widget builder from spawn_inner. Canonical root fields have no legacy root/Saver/PageSaver; use a narrowly named legacy fixture adapter only until Task 4 removes production use. Do not call spawn then disable threads. HostContext supplies local author to MainPanel; don't set the canonical actor or synthesize author from instance ID.
- [ ] Make canonical sync a pure document-to-Workspace reconciliation, including zero pages/tags. Remove “unknown local page/tag” push and grace/adopt decisions from this path. Preserve selected ID if it survives; otherwise choose first root ordered by `(order, page UUID)`, or None if empty. Update sidebar/search/links/tags from that projection. Existing intentional event actions run before the projection; never interpret a stale projected row as an edit to replay.
- [ ] Add canonical bind that creates the real BlockEditor, enables action interception, adopts existing block list and canonical marks, binds title and author, and accepts an empty list. No Markdown read/parse and no get-or-create document path. Avoid rebind when page unchanged so ordinary sync preserves caret. A newly created empty page can show the existing empty editor affordance, but its first durable block is an actual user edit action.
- [ ] Drain pending semantic block operations and inline formatting before select/open/duplicate/delete; queue only local intents if a callback lacks a mutable document, then consume them at the framework-bound sync with one action boundary. Reuse current tree-diff action logic, but snapshot current document to calculate it and rollback/reproject rejected optimistic metadata. Stale actions must visibly fail/reconcile, never leave a phantom local page to push later.
- [ ] Replace duplicate's Markdown copy with a new deterministic `NotesAction::DuplicateSubtree { source: String, pages: Vec<DuplicatePage> }`, where `DuplicatePage { source: String, destination: String, blocks: Vec<(String,String)> }` supplies fresh page/block IDs minted once before dispatch. Validate an exact source subtree/mapping, read all source bodies/kinds/native marks/tag edges from the action base document, copy using NotesDoc, remap internal links to cloned pages and preserve external links. Reuse existing insertion-after-source/title “ copy” behavior. Author action replay mints no IDs; it is one atomic domain action/history entry. Export helper `duplicate_subtree_action(doc: &AutoCommit, source: PageId) -> Result<NotesAction, String>` and register through existing notes.action decoder/executor; add serde roundtrip, stale-map and duplicate-ID refusal tests. The internal copy helper is `NotesDoc::duplicate_subtree(doc: &mut AutoCommit, source: PageId, pages: &[DuplicatePage]) -> Result<(), String>`; validate all inputs before any mutation.
- [ ] Drive create/rename/reparent/reorder/delete/duplicate, text splice, block split/move/checklist, marks, tag assignment and OpenPage/OpenTag through actual events and bound semantic addresses. Assert one history entry for each logical action, none for View operations, untouched outer shell record, same author and correct instance IDs. Revoke permission after discovery and require denial without mutation. Closing A must not cancel B's editing or deliver stale A results to reopened A. Keep existing domain resource IDs unchanged.
- [ ] Add explicit same-author interleaved history test: A edits root title, B edits child text, A assigns a tag; undo A removes only the tag action, undo B removes only B’s text edit, and redo each restores its own change. A edit→undo A→B edit→redo A preserves B and A’s redo. Mount B while A has a pending typing burst and prove mounting does not rebase/capture that action. First undo after import removes only the first user action, never imported content or receipt. Use the framework’s actual record UndoLog APIs and bound dispatcher, not a private Notes history model.
- [ ] Consume the common framework's actual `RecordStore::save_status(id) -> Result<RecordSaveStatus, RecordOpenError>` and add a default AppWidget hook `record_save_status(&mut self, status: ui::record::RecordSaveStatus)`, called under component-bound sync. Reuse `RecordSaveStatus::{Saved, Saving, Failed(String), MemoryOnly}`; Notes maps these to its real StatusModel, with MemoryOnly explicitly labelled session-only rather than durably saved. The common writer owns coherent acknowledged heads/error; Notes neither polls `get_heads`, flushes during status projection, nor starts a saver. Add hook/status tests for queued -> durable, stale older completion while newer edit pending, sticky failure through idle/no-op frames until full checkpoint succeeds, and unchanged pending typing transaction across repeated status queries. The actual asynchronous acknowledgement implementation belongs to prerequisite framework Task1; no new writer implementation is hidden in this Notes task.

- [ ] Run Task 2 focused tests and mandatory gates; self-review and commit `feat(notes): host the real notebook widget on canonical records`; controller reviews and verifies exact SHA on Fedora.

## Task 3: Typed local View with stable page/editor restoration

**Files:** new `apps/notes/src/local_view.rs`; `app.rs`, `main_panel.rs`, `sidebar.rs`, `lib.rs`; `ui/src/widget/app_widget.rs`, `ui/src/{application,application_view}.rs`;  `canonical_views.rs`, ui/application_contract tests.

**Consumes:** common AppComponent capture/restore signatures, SavedInstanceView/InstanceViewStore; Task 2 read-only projection. **Produces:** additive app-local payload hooks and SavedInstanceView V2, preserving existing View and FrozenApp wire.

```rust
// ui::application_view, opaque to framework but explicitly typed/versioned.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ApplicationLocalView {
    pub app_id: String,
    pub version: u32,
    pub payload: Vec<u8>, // app-local presentation only; never document bytes
}
// Added default methods on AppWidget; called under the same bound scope as View.
fn capture_application_view(&self) -> Result<Option<ApplicationLocalView>, ThawError> { Ok(None) }
fn restore_application_view(&mut self, _local: &ApplicationLocalView,
    _doc: &AutoCommit) -> Result<(), ThawError> {
    Err(ThawError::State("application-local restoration unsupported".into()))
}
// Default restore rejects Some payload as unsupported, rather than discarding it.
// Notes payload is serde_json bytes (strict schema); envelope remains postcard.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotesLocalViewV1 {
    pub selected_page: Option<PageId>, pub active_tag: Option<TagId>,
    pub drawer_open: bool, pub expanded_pages: Vec<PageId>, pub editor_scroll: f64,
    pub focus: NotesFocus, pub anchor: Option<BlockPosition>, pub caret: Option<BlockPosition>,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub enum NotesFocus { None, Sidebar, Title, Editor }
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BlockPosition { pub block_id: String, pub byte_offset: usize }
```

`NotesLocalViewV1`/enums live in Notes, not ui. Block positions persist existing stable block IDs; offsets are UTF-8 byte boundaries. No title/body/tag copies, execution state, pending action payloads or document snapshots. Editor scroll, sidebar expansion and drawer/filter state are local; NoteTreeView currently has no scroll offset, so this plan adds no imaginary sidebar scroll field; pages/tags/membership remain only in the record. Existing View viewport/theme handling remains framework-owned.

- [ ] Add failing local View roundtrip and stale-target tests. Two instances share one document, select different pages, scroll independently and retain distinct editor focus/carets. Persist both, mutate B's text/delete A's selected block/page, then reopen A with old local bytes; latest document remains, invalid target falls back locally and no durable mutation/history entry occurs. A schema/payload decode error returns retained ComponentSetup with original local bytes. Unknown app payload/version, malformed JSON, nonfinite/negative scroll and invalid UTF-8 boundary are rejected or safely clamped as specified below, never ignored silently.
- [ ] Extend SavedInstanceView in `application_view.rs` with `application: Option<ApplicationLocalView>` and write version 2. Preserve the original V1 postcard schema in a private `SavedInstanceViewV1` decoder; inspect envelope version first, fully decode with that exact schema, map V1 to V2 with None; version 2 decodes the new schema. Unknown/trailing/malformed bytes fail and remain untouched. Keep InstanceViewStore::load/save and AppComponent existing public capture_local_view/restore_local_view/close signatures; AppComponent retains app-local payload separately and close writes it into the saved envelope. RestoreInput::Local still takes SavedInstanceView. No changes to View/FrozenApp structures or their existing postcard fixtures, version constants or wire bytes.
- [ ] Extend AppComponent's checked local restore order: validate all envelope+app payload, project latest document, restore Notes selected page/sidebar expansion and build/bind editor, rebuild component focus topology, then restore its ordinary View and stable Notes focus/caret/scroll. Use two-phase validation/application inside the Notes hook so malformed input cannot partially alter even local state. Store decoded pending local restoration until layout can clamp scroll, with no action/effect replay. App-local stable selection is authoritative over stale generic focus ordinal if both exist; generic focus serves unmigrated apps. Capture/restore both contributions under the existing bound lifetime; closed mounts cannot publish restoration wakes.
- [ ] Implement selected fallback: keep valid selected page, otherwise first root `(order, UUID)` or None; missing active tag clears filter. Missing block collapses selection to start of first surviving block; empty page has no caret. Clamp valid offsets to current text length then down to a UTF-8 boundary when text shortened; reject structurally malformed/empty block IDs. Reject NaN/infinity and negative scroll; clamp finite scroll against current layout extent. Filter expanded_pages to surviving page IDs, sort/deduplicate, restore NoteTreeView expanded/did_initial_expand after its initial projection, and expand the selected page’s ancestors so selection is visible. Expose Notes-owned `NoteTreeView::expanded_pages() -> Vec<PageId>` and `restore_expanded_pages(&mut self, ids: &[PageId], workspace: &Workspace)` in sidebar.rs; these only rebuild local rows. Drawer open state uses existing SidebarDrawer::is_open/snap and is preserved locally across responsive layout; transient palette/menu/search query/drag progress closes on restore. Sidebar inline rename is committed through its normal action before capture or cancelled without replay, never serialized as document text.
- [ ] Capture current BlockEditor::document_selection()/focused_index and map indices to stable IDs. Use existing `select_document_range(DocumentPosition, DocumentPosition)` after mapping stable IDs to current indices; for a single caret use `focus_block(index, CursorPlacement::Byte(offset))`, reading the current TextInput cursor with `cursor_pos()`. These existing APIs manipulate presentation only, not TextSplice or a commit. Bind title/sidebar focus using NotesFocus then apply generic widget ephemeral caret only when it resolves to the same stable target. Tests inspect actual focused line and selected bytes after a layout rebuild, not merely decoded structs.
- [ ] Run Task 3 focused Notes/UI tests, existing freeze/thaw compatibility and mandatory gates; self-review and commit `feat(notes): restore independent notebook views`; controller reviews and verifies exact SHA on Fedora.

## Task 4: Standalone/headless cutover and canonical regression qualification

**Files:** `apps/notes/src/{main,component,app,shots}.rs`, `README.md`, old persistence/event fixtures and new component tests; `tests/it.rs`. No shell catalogue edits.

**Consumes:** Tasks 1–3, common LaunchSpec/App/Driver/TileHost component entrypoints, and the separately implemented SDK standalone host preparer with its exact validated record/profile/workspace CLI. That host bootstrap plan is a required prerequisite before Task4 can execute; this candidate does not invent its CLI or authenticate an app-supplied author string. **Produces:** one production factory across runners, explicit local import CLI, real production component tests; former spawn adapters restricted to deterministic legacy fixtures or removed from public production API.

- [ ] Add runner parity tests with identical explicit record inputs: `App::from_component` without a window, `TileHost::from_component` in an outer App, and `Driver::from_component`. Each uses `definition().instantiate`/LaunchSpec::prepare_component and exact registry/action addresses. Execute real Notes create/edit/tag/open plus save/drop/reopen with the same expected domain/resource outcomes and one history entry. Use independent isolated profiles for equivalent runner comparisons and the one shared owner only for same-record multi-instance cases. Include the actual notes binary's semantic headless path and exit/result JSON; a screenshot fixture alone cannot pass this gate.
- [ ] Add the production-binary test `canonical_component::headless_binary_creates_and_reopens_record`: the integration test itself runs through the isolated runner; launch `env!("CARGO_BIN_EXE_notes")` as its child with `--headless-drive` JSON containing `{"actions":[{"click_named":"notes.first_note"}],"dump":"<fixture>/record.json"}` (construct the JSON with serde_json, not shell interpolation). Assert exit 0, real page resource and saved canonical record; relaunch its existing target and assert that page persists. Provide the same explicit temporary XDG/config/cache/runtime roots to both child launches only and use the common runner’s accepted record-target CLI for reopen. Add malformed-headless JSON case asserting the established nonzero failure code. The fixture uses a controlled subprocess environment, never modifies process-wide env or HOME.
- [ ] Replace main's new_with_args/live_root/open_state/configure_app/peer_id paths with `qos_app::standalone::launch_spec(notes::definition())`, supplied by the standalone-host-bootstrap candidate and its required authenticated-component-session producer, preserving existing scenarios/shot handler/viewport/CLI outputs. The SDK standalone preparer owns explicit profile/workspace/identity/instance provisioning before factory construction; LaunchSpec invokes it once and then constructs the same definition. Definition-only launch returns MissingComponentHost and cannot pass this task. No `Autosave::disabled("Notes owns…")`, cwd/HOME read, process-wide author or standalone-only action registration remains in canonical factory code.
- [ ] Add the explicit standalone subcommand `notes import-legacy --source <absolute-directory> --state-root <absolute-profile-state-root> --workspace <workspace-id> [--source-key <64-lowercase-hex>]`. Notes main recognizes only this subcommand before calling the unchanged shared launch parser; parse its three required named values and optional source-key, reject malformed/duplicate/unknown flags or extra launch/recovery/headless options. Add `run_import_cli(args: &[String]) -> Result<ImportOutcome, ImportError>` in legacy_import.rs: validate absolute paths, construct explicit RecordOwners, open dev.qos.notes/workspace, use an explicit source-key without reading/canonicalizing source or compute source_key only when that optional argument is absent, then call import_legacy. Emit the stable source_key, durable destination ID and first-import selection in JSON, then exit; this command starts no app or native window. Existing component launch consumes its canonical record through the common target mechanism. Standard launch never scans ~/notes or imports. Print retained source_key and destination for durability/retry errors without claiming saved on failure. Test the actual CLI after a successful import and source-directory removal: supplying the emitted --source-key plus original absolute --source must reopen the matching receipt without filesystem access to source. Without --source-key, path-derived identity still requires that location to exist; document this limit instead of claiming the default command can rediscover a removed source. Test first flush failure with source intact and the same explicit key on retry, preserving source bytes and exact destination. This local migration CLI is not a host grant API and does not modify LaunchTarget/LaunchArgs or smuggle arguments through environment.
- [ ] Remove production callers of ambient `NotesApp::spawn_default` and legacy persistence constructors. Convert tests that claim app persistence to canonical owner fixtures; retain storage codec/PageSaver tests as explicitly legacy-format unit/integration tests until removed in a separate cleanup. Shots prepare deterministic fixture document and instantiate the same definition, preserving WELCOME_BODY only as an explicit screenshot fixture. Eliminate seed-grace bypass from canonical tests. If retaining legacy spawn APIs temporarily for existing test-only external harnesses, deprecate/document them as fixture compatibility only; no second production implementation remains.
- [ ] Update Notes README with canonical notebook semantics, explicit import command, preserved source/strict missing-page behavior, retry/conflict/changed-source policy and unsupported concurrent old-writer source snapshots. State that provider/collaboration availability is not proven by this component. Optional spellcheck absence must remain factual; do not show “available” merely because the manifest requests optional@1.
- [ ] Run full Notes suite, common affected component regressions, compile/lint/format and semantic headless binary checks on native; self-review then commit `refactor(notes): share the canonical factory across all runners`. Controller reviews and runs equivalent Fedora checks at that exact SHA. Production shell registration, Files discovery/opening, Mac window/IME/VoiceOver and encrypted cross-host collaboration remain later gates; do not claim their completion here.

## Test and commit gates

These commands are **future execution instructions**, not results of this planning task. Use the already established isolated helper. On native prefix every Cargo command below with:

```sh
CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py
```

On Fedora the controller uses the exact task commit and prefix `CARGO_TARGET_DIR="$PWD/target/notes-verification" python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py`. Preserve HOME/toolchain, existing lockfile, `--locked --profile ci`, and require nonzero discovered/executed counts. Record `rustc -vV`/host and exact git SHA on each host; use controller-established synchronization, not an invented VM command. Run formatting `cargo fmt --all --check` on both. Native tests/checks precede each explicit task source commit; controller review and exact-SHA Fedora gates follow it. A failed gate is fixed or reported with exact parent-SHA evidence, never counted as portable success.

At the audited Task 1 preimage, Notes has 121 source-declared library test identities and 27 source-declared integration identities in the four existing modules named below. These are source counts, not nextest discovery or execution results; `canonical_import` will add new integration identities. Task 1 must list and run the identical integration expression with all 27 existing identities plus the new import tests, then list and run the full unfiltered `-p notes --lib` target. A family selector cannot replace the full library gate.

| Task | Focused nextest payload | Mandatory check/lint payloads on both hosts |
| --- | --- | --- |
| 1 | `cargo nextest list --locked --profile ci -p notes --test it -E 'test(canonical_import::) \| test(markdown_marks_roundtrip::) \| test(markdown_phase3_roundtrip::) \| test(page_file_io::) \| test(workspace_persistence::)'`; `cargo nextest run --locked --profile ci -p notes --test it -E 'test(canonical_import::) \| test(markdown_marks_roundtrip::) \| test(markdown_phase3_roundtrip::) \| test(page_file_io::) \| test(workspace_persistence::)'`; `cargo nextest list --locked --profile ci -p notes --lib`; `cargo nextest run --locked --profile ci -p notes --lib` | `cargo check --locked -p notes --all-targets`; `cargo clippy --locked -p notes --all-targets -- -D warnings` |
| 2 | `cargo nextest run --locked --profile ci -p notes --test it -E 'test(canonical_component::) \| test(canonical_actions::) \| test(interaction_event_path::) \| test(tree_operations::) \| test(cmd_k_palette::)'` | `cargo check --locked -p notes -p ui -p qos-app --all-targets`; `cargo clippy --locked -p notes -p ui -p qos-app --all-targets -- -D warnings` |
| 3 | `cargo nextest run --locked --profile ci -p notes --test it -E 'test(canonical_views::) \| test(main_panel_scroll::)'`; `cargo nextest run --locked --profile ci -p ui --test it -E 'test(application_contract::)'`; `cargo nextest run --locked --profile ci -p ui --lib -E 'test(view::tests::)'` | `cargo check --locked -p notes -p ui -p qos-app --all-targets`; `cargo clippy --locked -p notes -p ui -p qos-app --all-targets -- -D warnings` |
| 4 | `cargo nextest run --locked --profile ci -p notes`; `cargo nextest run --locked --profile ci -p ui --test it -E 'test(application_contract::) \| test(bound_component_dispatch::) \| test(instance_undo::)'` plus registered production-binary semantic headless integration case | `cargo check --locked -p notes -p ui -p qos-app --all-targets`; `cargo clippy --locked -p notes -p ui -p qos-app --all-targets -- -D warnings` |

In actual shell commands remove Markdown's `\` escaping before each `|` inside the nextest expression. Use `cargo nextest list` with the identical selection first; fix spelling/registration rather than accepting an empty run. Task 4 keeps Notes renderer/golden tests under the existing isolated golden-tail policy and records actual GPU prerequisites/errors. Do not blanket-skip portable tests when an unrelated Mac platform dependency fails. The larger exact-commit native/Fedora workspace membership/all-target/example/complete nextest matrix remains the controller's milestone gate. After any normal push await automatic CI completion; use `fgj actions` on failures and keep relevant CI green before merging.

## Self-review and explicit handoff limits

- Schema and source trace: one multi-page record, no Markdown/workspace copy as ordinary authority; receipt and imported content share initialization/publication. Empty notebook/page and cleared native marks never seed again.
- Construction: one definition, unique initialization owner, no mounted source savers, exact common error retention and checkpoint retry; no fake file grant or app→shell edge.
- Actions: real Notes resource IDs, registry and semantic editor path; deterministic duplicated IDs in payload, current authority and explicit instance history context.
- View: selected page/filter/drawer/scroll/stable focus are local, no domain bytes; SavedInstance V1→V2 decoder, unchanged FrozenApp wire, retained malformed input.
- Meaningful tests: strict source errors, crash/flush retry, stale/denied actions, two-instance undo/persistence/local restore and all three real runners, registered in the actual `it` binary.
- Integration dependencies: common status hook and app-local View extension are bounded additions justified by actual Notes needs; catalogue/providers/transport/native window remain later consumers, not assumptions of support.

This candidate is ready for controller preflight and sequencing after its common-framework prerequisite. No implementation, compilation, VM validation, GUI launch or goal-completion claim accompanies it.

Controller consumer preflight: the standalone entrypoint is now concretely named by standalone-host-bootstrap-plan-draft.md, but that candidate requires an absent authenticated-component-session implementation. Notes Tasks 1–3 remain the real embedded/headless library slice; Task 4 production binary cutover is pending the actual accepted producer, including compatible Linux confined launch. A named function in a candidate is not an implemented prerequisite. HostContext preparation retains Rc<InstanceViewStore>; close uses that exact store and retains failed recovery reports.

Controller preflight: Tasks 1–3 are the canonical real Notes component slice after the common framework producer. Task 4 remains contingent on the actual accepted standalone bootstrap and authenticated-session producer; it must not block shell embedding of Tasks 1–3 or be marked complete from a library fixture. No production or native qualification has occurred from adopting this plan.
