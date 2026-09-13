# Explicit Shell Fixture Services Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (SDD), task-by-task, with a fresh implementer and independent spec/quality review for each task. One source author at a time; no implementer self-approves the next gate. Tasks 1 and 2 finish their native CPU safety/build/lint gates and independent reviews before Task 3. The controller owns Task 3 GPU execution on both native macOS and Fedora, after reviewing its committed conversion. Do not start until the active typography author releases source/build ownership. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the existing populated shell retained tests and `redraw_bench`, and the later empty-workspace GPU benchmark, construct the actual shared widgets from explicit fixture inputs without starting ambient services or reading user state.

**Architecture:** Add one explicit fixture constructor at the actual FrameBuilder producer and thread narrow values into its existing chrome/overlay constructors before any side-effectful defaults run. Keep optional discovery/polling providers genuinely absent, retain real Files against an owned directory and real in-memory records, and give Files explicit thumbnail/opener absence. Existing production constructors retain Linux behavior and delegate to the same widget initialization bodies.

**Tech Stack:** Existing Rust shell/UI/file-explorer crates, existing `RecordStore`, `HostedTileInputs`, `TextRenderer`, nextest and the isolated command wrapper. No new dependency, feature or generic service framework.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md`; fixture-audit requirements in `docs/superpowers/plans/2026-09-13-shared-retained-gpu-target.md` Task 3; controller task directing this standalone prerequisite. Source evidence: `.superpowers/sdd/2026-09-13-direct-shell-core-cutover/explicit-fixture-services-source-audit.md`.

Status: ADOPTED after controller preflight and bounded revision; no implementation or fixture runtime evidence yet. Source audit hashes reconciled at 114345a254bca7b026e4c4b5f92916bbb4e15af3. Typography renderer and fixture CPU contracts accepted; historical visual gate remains open. Inspected 2026-09-13 beginning at controller-supplied anchor `f574ac742`; controller subsequently reported typography source `b9105a23b` and passing Fedora CPU checks. Evidence records inspected file hashes because another author owns the checkout. Reconcile the new source anchor before implementation; no shell runtime has been run since the controller imposed the gate. No source/build/test/VM/GUI/network/git operation was performed. The old statement that the populated benchmark became safe at `4898888d` is disproven by the transitive constructor audit; existing timing receipts remain historical measurements, not isolation evidence.

## Global constraints

- “Benchmarks and tests must not load the user's real state or secrets.” HOME stays unchanged for toolchains and is never accepted as fixture data authority. An isolated XDG wrapper alone is insufficient.
- Preserve the populated demo tree, actual Notes/Files/Terminal/Document widgets, window sizes, actions, font ownership, retained/dense algorithms, damage assertions and benchmark thresholds. Do not substitute empty workspaces for the populated benchmark.
- Optional provider absence is explicit and truthful. No success-returning dummy service, magic environment no-op flag, synthetic Files/Notes implementation, or production-wide service disable.
- Production Linux `FrameBuilder::{new,with_tree,with_handle,with_handle_and_records}`, existing root constructors, `ShellOverlayHost::new`, `ChromeState::{new,new_with_handle}`, Files constructors and defaults keep their signatures and production behavior. Only explicit fixture callers select the new path. No `cfg(test)` substitution inside a production constructor.
- No broad real-app host/service port, Keychain, network stack, launcher migration or filesystem capability redesign. Browser/live app placeholders outside the approved fixture inventory are rejected, never silently replaced.
- This candidate grants runtime scope to the audited fixture/action closure and focused tests only. It does not certify arbitrary shell actions or the entire existing shell test suite. Full-suite execution stays blocked for unconverted legacy constructor callers; the evidence includes their lexical inventory. Do not hide that limitation with a filter described as full coverage.
- This is a standalone prerequisite to repeat GPU Task 2's runtime acceptance and begin GPU Task 3. It neither depends on running those unsafe parents nor implements the new common benchmark.

## Audit findings and chosen boundaries

| Actual source path | Observed authority/start | Candidate treatment |
| --- | --- | --- |
| `FrameBuilder::with_tree -> build_nirios_overlay_host_with_chrome -> ...with_handle -> ...with_inputs` | HostedTileInputs fixture covers Files startup only; no live status poller, but all other default leaves still run | Add explicit fixture sibling through the same root builder; leave old route untouched |
| `frame.rs:460` `from_parts` | `TextRenderer::new` system/configured font discovery; settings path metadata even with watcher off; `state::local_user_id` | Consume explicit renderer, theme and local principal before layout; no settings stamp and watcher off |
| `ChromeState::new_with_handle`, `QuickSettingsState::default` | Identity public-key path/USER fallback; settings night light; privacy `/sys`, `/dev`, audio/process/PipeWire discovery | Explicit principal and fixed QuickSettingsState with unavailable hardware readings; preserve real default on old route |
| `ChromeState::local_user_for_room` and `add_workspace` | Per-room registry lookup, fallback to ambient identity; owner adoption invokes same resolver | Explicit in-memory principal resolver mode for fixture chrome; active-local and owner adoption use it |
| `ShellOverlayHost::build_search_registry` | Eager catalogue cache read + helper process refresh, Files name walker, Tantivy body walker/cache, rank worker | Fixture registry contains existing Notes, Editor and Records providers only. Records index is the actual `RoomRecords::in_memory`. Files/app/host-app/live-app/general external-action providers are absent, not constructed then discarded |
| Applications/HostApplications query refresh | Host desktop files, installed app catalogue snapshots and background snapshot builders; activation launches apps | Absent from this narrowly scoped fixture; no fake result lists |
| `FilesProvider::new/rescan`, `FilesIndex::open_or_create` | HOME walk, ambient cache lookup, detached name/rescan/rank workers and retained-but-unjoined body handle | Never called by fixture; instrument these real start entries in shell unit tests before side effects to prove absence. No FilesIndex refactor needed for this option |
| Overlay startup | Shortcut config load, persisted QuickSettings layout, persistent room-history frontiers | Pass default config values; construct `QuickSettingsRoot::with_config`, `RoomActivityPane::new` directly |
| Overlay keyboard paths at ~8272/~13441; shortcut editing | `DesktopControlsConfig::load` on events, shortcut `.save` on edit | Store an explicit fixture desktop-controls value; reads use it. Fixture shortcut edit validates and changes local config only; old route retains disk load/save |
| `prepare_quick_settings_transition` | Opening QS starts fast + firmware workers; drilling Wi-Fi/Bluetooth starts scans | `QsRefresh::without_os_reads()` creates channels but no worker; installed absent dispatcher returns errors for EVERY trait method including ambient default methods |
| `ProjectionOverlay::default -> ProjectionController::default/new` | Always starts `qos-projection` worker; later discover/connect invokes configured helper | Add `ProjectionController::unavailable()` with no worker/channel sender and explicit error; feed existing overlay via `from_controller` |
| `FileExplorerWidget::with_startup_inputs` | Startup dir and account/record inputs are explicit, but thumbnail service still opens shared `/tmp/qos-file-explorer-thumbnails` and starts four workers | New explicit Files services constructor; fixture passes no thumbnail service. `Welcome.txt` uses a type badge already, so no fixture thumbnail pixels are removed |
| Files document activation | Thread-local global opener captures strong Chrome; absent opener falls back to `open::that`/desktop | Explicit instance opener state; absent service returns a visible failure and NEVER consults global/standalone fallback. Fixture root does not install the global opener |
| Files reload / metadata | Injected startup accounts/identity guard already suppresses reload; actual listing still calls ambient `is_workspace_path` for xattr classification | Add explicit workspace-roots listing helper and use it for injected startup, reload and metadata paths; empty fixture roots cause no FUSE xattr/network request |
| `HostedTileInputs` | Arc-owned temp directory removed on last drop; rebuilds clone input; existing fixture Terminal uses null backend | Preserve directory ownership; add explicit `fixture_in(parent)` creation, retain in Chrome and rebuilt Files inputs. No shell process/PTY spawn for fixture Terminal |
| Actual Notes/Editor/Document placeholders | `shell::notes_app::NotesApp`, `EditorApp`, WordProcessorWidget have in-memory constructor state; records bound in existing GroupRow path | Preserve exact constructors and tab record binding. This is not qualification of standalone `apps/notes::NotesHost::spawn_default` |
| Browser placeholder | Live engine + persisted settings/history/bookmarks | Reject a Browser placeholder in fixture validation; existing populated baseline has none |
| Status/notification/session-audio/peer/broker/onboarding/portal attachments | Explicit attachment methods start live services or enable their consumers | None attached in fixture. Tree has no persistence/ACL path. Existing fixture picker scope remains bounded. No global document opener or launcher identity projection publication |
| Root widgets / action closure | Topbar, sidebar, overview, capture widgets, WorkspaceTree/RecordStore and in-memory launcher-event queues | Reuse actual widgets. Global launcher-event polling must be absent on fixture host so another host cannot inject a launch result/notification. Captures, tour, projection, external-action activation are outside the approved two-control sequence |

Files `StartupInputs` are data, not a sandbox. This candidate only executes the owned synthetic directory and the audited QS/overview sequence; it does not claim arbitrary Files navigation cannot leave the root. General-purpose constrained file authority belongs to the later real-app service contract. Tests of this candidate may list/refresh the owned root and attempt a document activation with the explicit absent opener; do not drive “up”, remote connects or arbitrary filesystem operations. Reject supplied fixture trees with `persist_path`, `fs_acl_path`, foreign/peer/local app tiles or unsupported placeholders before constructing ANY widget.

## Exact producer / consumer contracts

All names below are proposed additions. They are deliberately narrow and do not replace production APIs. A private initialization struct factors each existing large constructor once; no copied widget tree or alternate renderer.

```rust
// shell/src/shell/fixture_inputs.rs (public module exported by shell/mod.rs)
pub struct ShellFixtureInputs {
    // Private: owned HostedTileInputs, fixed configs/principal, renderer, theme.
    // Not Clone: TextRenderer is moved into its one actual FrameBuilder.
}
impl ShellFixtureInputs {
    pub fn new(
        fixture_parent: &std::path::Path,
        text_renderer: ui::text_render::TextRenderer,
        theme: ui::theme::Theme,
    ) -> std::io::Result<Self>;
}
// Errors remain data; no fallback constructor on error.
#[derive(Debug)]
pub enum ShellFixtureError {
    InvalidTree(&'static str),
    Hosted(crate::shell::hosted_inputs::HostedTileError),
}
// Implement Display + std::error::Error; Hosted preserves its source.

// shell/src/shell/frame.rs
impl FrameBuilder {
    pub fn with_fixture_inputs(
        tree: crate::shell::state::WorkspaceTree,
        viewport: (u32, u32),
        inputs: crate::shell::fixture_inputs::ShellFixtureInputs,
    ) -> Result<Self, crate::shell::fixture_inputs::ShellFixtureError>;
    fn from_parts_with_presentation(
        parts: (ShellOverlayHost, Chrome),
        viewport: (u32, u32),
        presentation: FramePresentationInputs,
    ) -> Self;
}
struct FramePresentationInputs {
    text_renderer: ui::text_render::TextRenderer,
    theme: ui::theme::Theme,
    own_identity: String,
    theme_store_stamp: Option<std::time::SystemTime>,
}
// Old from_parts computes exactly its old values then calls this body.
// Fixture never calls old from_parts and uses None for theme_store_stamp.

// shell/src/shell/mod.rs; private sibling path retains root building order.
pub(crate) fn build_nirios_overlay_host_with_fixture_inputs(
    tree: crate::shell::state::WorkspaceTreeHandle,
    inputs: crate::shell::hosted_inputs::HostedTileInputs,
) -> Result<(overlay_host::ShellOverlayHost, chrome_state::Chrome),
            fixture_inputs::ShellFixtureError>;

// shell/src/shell/chrome_state.rs
impl ChromeState {
    pub(crate) fn new_with_fixture_handle(
        tree: WorkspaceTreeHandle,
        inputs: super::hosted_inputs::HostedTileInputs,
    ) -> Chrome;
}
// Private field: identity_source: IdentitySource
// This is a principal lookup choice, not a general service registry.
enum IdentitySource { SessionRegistry, Explicit(String) }
// Existing local_user_for_room/add_workspace consult this same field.
// Explicit principal is "shell-fixture", never USER/QOS_USER_ID.
impl QuickSettingsState { pub(crate) fn fixture() -> Self; }

// shell/src/shell/overlay_host.rs
impl ShellOverlayHost {
    pub(crate) fn with_fixture_content(content: Box<dyn ui::widget::Widget>) -> Self;
}
// One private initialization struct carries search_registry + room_records,
// shortcut_config, desktop-controls source, qs_root, qs_refresh, qs_actions,
// room_activity and projection_overlay. Existing new resolves production
// values; with_fixture_content resolves explicit values; both use one body.
// Private DesktopControlsSource::{Persisted, Explicit(DesktopControlsConfig)}
// controls event-time reads; fixture shortcuts are never persisted.

// shell/src/shell/qs_refresh.rs
impl QsRefresh { pub(crate) fn without_os_reads() -> Self; }
// Same actual spawn function and `enabled` check; initialized false.
// shell/src/shell/projection.rs
impl ProjectionController { pub(crate) fn unavailable() -> Self; }
// tx becomes Option<Sender<WorkerCommand>>; None means unavailable.
// No worker, helper resolution or helper invocation on None. Existing
// new/default construct Some and retain their current worker behavior.
// shell/src/shell/projection_overlay.rs
impl ProjectionOverlay {
    pub(crate) fn from_controller(controller: ProjectionController) -> Self;
}

// apps/file-explorer/src/files_app.rs; exported via existing module
#[derive(Clone)]
pub struct HostedFileServices {
    pub thumbnails: Option<std::sync::Arc<thumbnail::ThumbnailService>>,
    pub document_opener: Option<std::rc::Rc<std::cell::RefCell<DocumentOpener>>>,
}
impl FileExplorerWidget {
    pub fn with_startup_inputs_and_services(
        inputs: StartupInputs, services: HostedFileServices,
    ) -> Self;
}
// Private per-widget FileServices::{Legacy, Hosted(HostedFileServices)}.
// Legacy keeps current constructors, thumbnail/cache fallback and TLS opener.
// Hosted consumes exact options; None NEVER means legacy fallback.
// For Hosted(None), open-file returns Err("Document opening is unavailable")
// to existing status presentation and does not call CommandRunner.

// apps/file-explorer/src/model.rs: real listing, explicit classification only
pub(crate) fn read_dir_with_workspace_roots(
    path: &std::path::Path,
    roots: &[std::path::PathBuf],
) -> Result<Vec<Entry>, String>;
// Delegate to read_dir_inner(path, path is under any supplied root).
// All injected FileExplorer read/reload/metadata paths use this helper.

// shell/src/shell/hosted_inputs.rs
impl HostedTileInputs {
    pub fn fixture_in(parent: &std::path::Path) -> std::io::Result<Self>;
}
// Existing fixture() keeps its legacy service selection and temp_dir parent.
// Both use one private directory-creation body; only fixture_in selects the
// explicit Hosted(None,None) services. Add private optional HostedFileServices.
// Fixture mode must fail RequiredServiceMissing if files inputs are lost,
// on Linux as well as Mac; it must never reach FileExplorerWidget::new.
```

`QuickSettingsState::fixture` spells out the existing deterministic presentation values: brightness `0.6`, volume `0.4`, wifi `true`, bluetooth `false`, bluetooth_available `false`, no wired/VPN/firmware/network/device lists, balanced power, root navigation, night light `false`, `PrivacyStatus::default`, `RadioState::default`. Battery is an explicit absent `BatteryInfo` literal with `present:false`, `capacity:0`, empty status, `on_ac:false`, and all optional capacities/cycle/limit `None`; do not call `from_root`, even on a supposedly nonexistent path. These are presentation inputs, not a claim that host hardware exists or has those settings.

Use a `pub(crate) UnavailableQsActions` implementing the existing Dispatcher. Every method returns `Err("Quick Settings service is unavailable".into())`: `set_brightness`, `set_volume`, `set_wifi`, `set_bluetooth`, `set_wired`, `set_airplane_mode`, `set_hotspot`, `set_mobile_broadband`, `set_vpn_connection`, `connect_wifi`, `set_power_profile`, `toggle_mute`, `set_mute`. The controller verifies each implementation is an explicit Err before the method-invocation test is permitted. Overriding only required trait methods is unsafe because several provided defaults call real OS helpers; they can do that work before an eventual Err. Do not modify the shared trait or use a recording-success implementation in the benchmark.

### Ownership and lifecycle

`ShellFixtureInputs::new` creates a unique child under an existing explicit absolute parent, writes precisely the current `Welcome.txt` bytes (`Shell capture fixture\n`), and rolls back on failure. The parent must be a freshly created private directory provided by the invocation/test owner, not HOME/cwd/root. Record parent and child separately. The widget receives only the child; it never falls back to parent or cwd.

The existing `Arc<FixtureDirectory>` is held in Chrome's HostedTileInputs and cloned across rebuilds. FrameBuilder owns host + Chrome; no fixture global opener captures Chrome, avoiding the current TLS strong-reference retention. HostedFileServices contains no callback/service in the baseline, so it starts no workers and adds no ownership cycle. Scope each builder before dropping the external temp parent. An external clone of Chrome intentionally extends child lifetime; tests must prove child persists while that clone lives and disappears after the final clone is dropped. Do not claim the parent's remove_dir_all proves child lifecycle.

Absent search providers never allocate/start Files workers or Tantivy readers/writers. Existing production FilesIndex JoinHandle storage does **not** join on drop; do not claim it does. We deliberately avoid introducing worker cancellation/join machinery by not starting those optional providers. Projection absence starts no thread; QsRefresh absence only owns channels. Retain existing null Terminal backend and prove no process start. Existing live projection/search/status worker shutdown semantics are unchanged and outside this candidate.

The renderer is constructed by caller using the controlled catalog, moved once into FrameBuilder and used for initial relayout and every later scene. Fixed theme enters before construction. No ambient default renderer may run briefly before setter replacement. The accepted typography source changes must land first; reuse their catalog APIs without editing font code here.

## Source-only permit checklist before the first fixture runtime

Every box is checked against actual constructor call sites at the new candidate SHA, before any shell test process runs. Compile/list commands do not prove these conditions.

- [ ] `with_fixture_inputs` validates tree/inputs first and calls only the explicit constructor path; old `from_parts`, Chrome default and overlay new are unreachable from it.
- [ ] The supplied renderer is installed before initial layout; no `TextRenderer::new`, configured theme, settings metadata, HOME/cwd discovery or local-key read is performed first.
- [ ] Initial and event-time identity, shortcut/desktop-controls, night-light, history and QS configuration all use explicit values. Active-local lookup and workspace addition cannot call the identity registry.
- [ ] Fixture registry construction contains no call to catalogue, FilesProvider, FilesIndex, ApplicationsProvider, HostApplicationsProvider or live broker. No provider can be reinserted by fixture startup/rebuild.
- [ ] The actual QsRefresh spawn gate starts disabled before first paint; controller source-verifies all 13 unavailable Dispatcher implementations are explicit Err bodies overriding ambient trait defaults before any method-invocation test runs. Projection unavailable construction does not create a live worker first.
- [ ] Files starts through explicit services before any thumbnail open; document activation never consults TLS/desktop fallback; injected read/reload/metadata classifies workspace paths solely from supplied roots. Empty remote/record roots retain their explicit meaning after reload.
- [ ] Root rebuilds retain HostedTileInputs and fixture principal/service modes. No fixture TLS opener or launcher projection publisher runs; ambient launcher queues are not pumped into fixture chrome.
- [ ] Tree persistence/ACL paths are absent and no notification/status/audio/peer/portal/broker/onboarding attachment runs. Browser/foreign/peer/live placeholders fail before construction. Only the reviewed two-action sequence is admitted by benchmark support.
- [ ] File-explorer deny hooks precede thumbnail cache/path construction, TLS opener access, SystemRunner process/desktop calls, and ambient is_workspace_path discovery; allowed owned listing observations remain separate. Every selected injected_startup_tests case arms them before construction. Test guards sit before real ambient reads/worker starts, use process-wide atomics (so accidental child threads cannot escape a thread-local flag), and cannot suppress work or fabricate results. They panic before side effects. Missing/late guard placement blocks the test, not just its assertion.
- [ ] Selected test inventory contains only audited fixture callers. Neither `test(shell::frame::)` broadly nor a full shell suite is permitted: OOBE and other unrelated paths need their own audit.

### Cross-crate test-instrumentation boundary

Shell `cfg(test)` guards instrument shell's actual library-test harness only. They do not instrument `os-actions` or `file-explorer` when those crates are compiled as normal shell dependencies. A zero shell counter therefore does not prove no underlying dependency process or filesystem call occurred. The separate file-explorer library-test gate exercises its own fail-before-I/O guards. Before shell fixture CPU tests, source review must verify the already accepted explicit Files constructor/service path remains selected and all 13 unavailable Dispatcher methods are explicit Err bodies. The method-invocation test checks actual unavailable behavior after that source permit; it is not a safe detector for a forgotten ambient trait default. Do not introduce a feature, replacement service framework or cross-crate interception mechanism to change this boundary.

### Concrete test skeletons and narrow test access

Add these exact shell test-support contracts under `#[cfg(test)]`; the fixture API remains available in normal example builds:

```rust
pub(crate) struct DenyAmbientStarts;
impl DenyAmbientStarts {
    pub(crate) fn new() -> Self;
    pub(crate) fn attempts(&self) -> usize;
}
pub(crate) fn ambient_start(boundary: &'static str);
// ambient_start increments one process-wide AtomicUsize; while guarded it
// panics with the boundary BEFORE the actual read/spawn. Drop disarms the
// guard. These are assertions, not substitute IO/service implementations.
// New host test accessor returns actual registry providers_iter().map(id):
impl ShellOverlayHost {
    pub(crate) fn fixture_provider_ids(&self) -> Vec<&'static str>;
}

#[test]
fn fixture_constructor_does_not_run_ambient_defaults() {
    let parent = tempfile::tempdir().expect("private fixture parent");
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/assets/fonts"),
        "en-US",
    );
    let renderer = ui::text_render::TextRenderer::with_font_catalog(&catalog)
        .expect("controlled fonts must load");
    let inputs = ShellFixtureInputs::new(parent.path(), renderer, ui::theme::Theme::dark())
        .expect("fixture files");
    let guard = crate::shell::test_support::DenyAmbientStarts::new();
    let mut frame = crate::shell::frame::FrameBuilder::with_fixture_inputs(
        crate::shell::state::WorkspaceTree::bootstrap_demo(), (3840, 2160), inputs,
    ).expect("fixture constructor");
    assert_eq!(frame.host().fixture_provider_ids(), vec!["notes", "editor", "records"]);
    assert_eq!(frame.chrome().active_local_user(), "shell-fixture");
    assert!(!frame.paint().encoding().is_empty());
    assert_eq!(guard.attempts(), 0);
}

#[test]
fn fixture_projection_unavailable_never_starts_worker() {
    let guard = crate::shell::test_support::DenyAmbientStarts::new();
    let mut controller = crate::shell::projection::ProjectionController::unavailable();
    controller.refresh();
    controller.connect_selected();
    controller.disconnect();
    assert!(matches!(controller.status, crate::shell::projection::ProjectionStatus::Error(_)));
    assert_eq!(guard.attempts(), 0);
}
```

Add action/rebuild assertions to the same actual constructor test using the existing benchmark click driver and target IDs `topbar_tray_pill` and `topbar_overview_button`. Do not create a replacement action state machine. Tests start the real no-op-free provider paths: fixture creation/pump/search, QS transition, projection refresh, Files activation/reload and root rebuild. For the guard itself, one unit test calls `ambient_start("guard-self-test")` inside `catch_unwind` and verifies it trips before a following counter increment; it does not call a production ambient constructor just to test the guard.

## Execution ownership and review gates

The controller follows SDD with sequential source ownership and records each task SHA, author report, independent review and ruling. The controller reviews test-guard placement before author CPU RED execution; RED may only be a compile failure or a fail-closed assertion before ambient I/O. The implementer then completes native CPU tests/checks/format/test-harness lint, self-reviews and commits. An independent reviewer checks spec compliance and code quality; the controller accepts or returns the task for correction before dispatching its consumer. No author may infer GPU permission from their own CPU green result.

Task 1's accepted output is explicit Files services/listing/owned-directory behavior with CPU safety evidence. Task 2 consumes that accepted SHA and outputs the explicit FrameBuilder constructor with CPU actual-start evidence. Task 3's source author only converts the selected consumers, runs reviewed CPU fixture checks, discovery, compile/lint and commits; its independent review checks every selected runtime entry before the controller runs native retained/GPU gates, mirrors the same SHA, runs Fedora gates and then the populated benchmark. Controller-owned runtime failures go back to the current source owner with exact evidence and a fresh review/validation cycle. Completion is controller acceptance of the applicable exact-SHA gates, not author self-approval. No implementation/runtime delegation is performed during this artifact-only planning revision.

## Task 1: Fix the actual injected Files leaves

**Files:** Modify `apps/file-explorer/src/{files_app,model}.rs`, `shell/src/shell/hosted_inputs.rs`, and fixture branch of `shell/src/shell/mod.rs`. Tests stay in `files_app.rs`'s `injected_startup_tests` module and new `hosted_inputs.rs` test module. No thumbnail crate changes or new worker implementation.

**Consumes:** Existing `StartupInputs`, `DocumentOpener`, ThumbnailService and `HostedTileInputs` ownership. **Produces:** exact HostedFileServices, explicit listing and fixture_in contracts above.

- [ ] Add failing tests `injected_services_absence_never_builds_thumbnails_or_uses_global_opener`, `injected_listing_and_refresh_use_only_explicit_workspace_roots`, `fixture_directory_lives_through_last_input_clone`, and `fixture_missing_files_input_does_not_fall_back_on_linux`. Tests use only synthetic temp siblings. For opener proof, install a test TLS closure which increments a counter and fails; activate actual `Welcome.txt` via the real widget command path and assert visible unavailable error and zero TLS/runner calls. Clear test TLS on scope exit. Do not use an actual launcher process.
- [ ] Add a process-wide file-explorer deny guard, not a counter-only observer. `#[cfg(test)] pub(crate) mod fixture_io_guard` lives in `model.rs`, shared by Files construction and model functions. Its `DenyAmbientIo::new()` arms an AtomicBool before the real widget is constructed; its Drop disarms it. `ambient_io(boundary: &'static str)` increments AtomicUsize and panics while armed BEFORE any legacy work. Put it first in `build_thumb_service` (before cache/path/open), `ask_host_to_open` (before TLS access), every `SystemRunner::{spawn,spawn_arg,open_default}` (before process/desktop invocation), and `model::is_workspace_path` (before `workspace_roots`/environment discovery). Calling a broken injected path therefore fails before shared cache reads/writes, desktop opening or ambient path classification. The guard is process-wide so a worker cannot bypass a thread-local flag; it never suppresses I/O or returns a fake result.
- [ ] Keep permitted listing evidence separate: a test-only `observe_owned_listing(path: &Path, in_workspace: bool)` immediately before the real `read_dir_inner` access records paths/classification and, while this test's owned-root observation is armed, panics before `read_dir` if the path is outside its synthetic child root. The observer authorizes no ambient fallback and never calls `is_workspace_path`. Tests assert actual startup/reload/metadata listings are under the owned child and `in_workspace == false`. They can inspect those fixture files normally. Fail-closed guard tests use `catch_unwind` around the assertion hook followed by a local counter and verify the following code was not reached; never call an unguarded production constructor to obtain RED. Nextest gives each test its own process; no simulated replacement filesystem/provider.
- [ ] Before ANY runtime in `injected_startup_tests`, arm the process-wide deny/owned-listing guards in every selected test, inspect that each hook precedes side effects, and convert the existing construction to `with_startup_inputs_and_services` with explicit absent services; its current `with_startup_inputs` call still starts ambient thumbnails. Implement private FileServices mode and route actual initialization/activation/listing through it. Existing public constructors delegate Legacy exactly as before. Add `fixture_in` and preserve existing fixture content and error rollback. Fixture Files uses hosted service absence before its constructor; fixture missing Files data returns an error before any Linux fallback.
- [ ] Run the Task 1 nextest selection only after inspecting the above probe positions and red test safety. Before compiling the full candidate, `cargo nextest list` is build/discovery, never an execution permit. Verify tests fail at the explicit boundary/missing API, not by contacting ambient services. Run focused native CPU checks plus file-explorer all-target Clippy and the shell library-test harness Clippy when shell test code changed; inspect the diff and commit the bounded candidate. Independent spec/quality review and controller acceptance of this exact SHA are required before Task 2; author self-review alone is not acceptance.

## Task 2: Construct FrameBuilder with explicit chrome and overlay inputs

**Files:** Create `shell/src/shell/fixture_inputs.rs`; modify `shell/src/shell/{mod,frame,chrome_state,overlay_host,qs_refresh,qs_actions,projection,projection_overlay}.rs`. Add fixture tests in `fixture_inputs.rs`. Test-only pre-side-effect counters go in `shell/src/shell/{catalogue,status,state}.rs`, `shell/src/shell/search/providers/files.rs`, `shell/src/shell/search/files_index.rs` and ambient constructor resolver entries in frame/chrome/overlay. All test-only instrumentation is local and absent from release. Reuse `shell/src/shell/test_support.rs` to hold shell counters and their guard, without environment hooks.

**Consumes:** Task 1 inputs/services; existing controlled TextRenderer; exact contracts above. **Produces:** `FrameBuilder::with_fixture_inputs`, reusable by both populated and empty-workspace producers.

- [ ] Add fixture-construction tests before implementation. `fixture_constructor_and_actual_overlay_actions_start_no_ambient_services` arms an ambient-start guard, constructs the real populated FrameBuilder, pumps actual widgets, opens/closes QS/overview via widget registry controls, and rebuilds its root. Assert the actual registry provider IDs are `notes`, `editor`, `records`; no file/app/host-app/action/live broker providers appear. A records search uses the same in-memory store/room index, not an invented empty success service. Add `fixture_empty_workspaces_use_same_root_without_app_services` using three empty named workspaces.
- [ ] Guard the real boundaries before any read/spawn: production Frame presentation resolver, Chrome default/identity resolver, overlay production constructor resolver, catalogue available_apps, FilesProvider new/rescan/rank-worker start, FilesIndex open, status spawn, projection live constructor and QsRefresh spawn. Within fixture test mode, an ambient attempt panics synchronously before side effects; the QS spawn probe runs after its enabled check to distinguish an attempted unavailable read from an actual OS work start. Guard reset uses RAII. No normal runtime constructor is run to obtain a red fixture baseline.
- [ ] BEFORE running `fixture_unavailable_qs_overrides_every_dispatcher_default`, the controller source-verifies all 13 Dispatcher methods have explicit unavailable Err bodies, including every provided default; only then invoke all 13 methods with harmless arguments and require Err. Shell test counters do not intercept os-actions dependency process calls and must not be reported as such. Add `fixture_projection_unavailable_never_starts_worker`: call refresh/connect/disconnect on actual unavailable controller, require explicit unavailable/error state and zero worker starts. No fake successful devices.
- [ ] Factor the actual constructor bodies as contracted. Test fixture uses explicit principal, pure QS state, default shortcut/QS/desktop-controls configs, `RoomActivityPane::new`, unavailable projection/QS, and in-memory Notes/Editor/Records providers. Do not call an old constructor and then overwrite fields. `with_fixture_content` must skip catalogue refresh entirely. Per-frame fixture host skips global launcher events; no live attachments are created.
- [ ] Route frame initial relayout through the supplied renderer/theme; metadata stamp is None. Route Chrome room actor and owner adoption through IdentitySource. Fixture root builder skips TLS document-opener install and launcher trusted-projection publication; actual root/GroupRow/sidebar construction remains the same function. Validate tree and inputs before any widget creation; later `try_to_ui_tile` checks fixture missing services instead of falling back. Browser and foreign/peer/live placeholders return InvalidTree/required-service error, not a stub.
- [ ] Add `fixture_rebuild_keeps_explicit_actor_records_and_services`, `fixture_directory_lives_through_rebuild_and_last_chrome_clone`, and `fixture_rejects_persistent_or_unsupported_tree_before_start`: owned temp data only, plus a Browser placeholder rejected before Browser::new. Exercise active_local_user/add_workspace to ensure registry fallback is not restored on a new room. Assert Notes edits survive root rebuild through the actual RecordStore and Files still lists Welcome.txt.
- [ ] Run CPU-only fixture selections, shell production checks and actual library-test harness Clippy below; file-explorer all-target Clippy remains required when its code changes. Self-review and commit this self-contained prerequisite. Independent spec/quality review must trace the exact source call graph at that SHA against the audit table, and the controller must accept it before Task 3 conversion. No GPU benchmark or shell GPU test may run under this author task; CPU green does not grant that permit.

## Task 3: Convert and qualify only the audited existing workload consumers

**Files:** Modify `shell/src/shell/frame_retained_tests.rs`, `shell/src/shell/render/paint.rs`'s `native_4k_framebuilder_actions_match_current_staging_for_one_and_two_targets`, and every FrameBuilder construction in `shell/examples/redraw_bench.rs`. Add `shell/examples/support/populated_shell_fixture.rs` only if needed to share the example's many constructor sites; it calls the public fixture API, never copies UI. Selected retained tests use a helper in `shell/src/shell/test_support.rs`. Unrelated frame tests (including OOBE) remain outside this runtime selection. Modify `.config/nextest.toml` only to place new actual GPU tests in existing exclusivity scheduling. No manifest/lock change is expected.

**Consumes:** Task 2 constructor and controlled catalog. **Produces:** same populated benchmark/action producer and existing retained oracles using explicit inputs, plus isolated native/Fedora receipts. The later `shared_gpu_bench` uses this public seam but is not created here.

- [ ] Replace every selected construction, including diagnostic/example tests, before executing any selected binary. Helpers create a TempDir owner in the calling scope, resolve `ui/assets/fonts` from `CARGO_MANIFEST_DIR`, build `FontCatalog::controlled_fixture(root, "en-US")` and `TextRenderer::with_font_catalog(&catalog)`, then call with_fixture_inputs with unchanged `WorkspaceTree::bootstrap_demo()`. Unit helper keeps its parent TempDir alive alongside the builder. Example support owns both via a small `PopulatedShellFixture { frame: FrameBuilder, parent: tempfile::TempDir }` test/example-only wrapper. `tempfile` is already a shell dev-dependency and available to examples; do not add a dependency. No changes to paint or event routing.
- [ ] Pin a semantic fixture inventory test: Home/Research/Design, tabs Notes/Files/Terminal/Document, Home active, Notes active, wide group, existing invented collaborator labels/colors/roles, current Welcome.txt bytes, in-memory records, same target control IDs and 3840×2160 extent. IDs may be generated, so compare relationships and labels, not fresh UUID equality. The empty-workspace benchmark remains a distinct future profile.
- [ ] Preserve benchmark action count 1,000, action ordering, warmup, normal motion, statistical summary and mean ≤25 ms / p99 ≤50 ms thresholds; retained tests keep reduced-motion setting, damage coverage and `mismatches <= 16 && max_delta <= 1`. Capture source diff confirming changes are fixture construction only. Old timers may include nondeterministic hidden background work; identify new receipts as `existing-populated-redraw-explicit-services-v1`, never relabel old unsafe receipts.
- [ ] After Task 2 acceptance, Task 3 source conversion/CPU/lint completion, commit, and independent review of its selected entry points, the CONTROLLER runs existing retained/dense pixel tests on the NEW safe fixture; compare both render paths at one candidate SHA. Assert QS/overview state changed, scenes nonempty and actual before/after pixels changed. Keep both one/two-target staging oracle and root-rebuild oracle. Do not render the unsafe parent as an A/B control. Existing saved parent images can be inspected only if already available and provenance is known; absence of those images limits old-versus-new pixel claims.
- [ ] Prove layout/workload preservation through source invariants plus new retained/dense oracle, and distinguish it from historical host-sensitive pixels. Optional search providers are not painted by the closed palette; absent thumbnails do not change the fixture's text-file badge. QS hardware/config values previously varied by host, so exact equality to an arbitrary old host run cannot honestly be guaranteed. No baseline regeneration or tolerance relaxation follows automatically.
- [ ] Task 3 native source author completes only reviewed CPU fixture checks, discovery, compile/format and production + actual library-test harness Clippy, commits the conversion and reports exact SHA. The independent reviewer and controller inspect source/lifecycle/test entry points before any GPU execution. The controller owns the native GPU retained/dense gates, mirrors that committed SHA, and owns Fedora all-target lint and matching CPU/GPU selections. Only after these safety and retained/dense gates pass does the controller run the existing populated release benchmark on both hosts and resume GPU Task 2 acceptance. Broader shell full-suite and GPU Task 3 remain separate audited runtime decisions.

## Command gates (execution instructions; NONE run during planning)

All commands run from the repository root. Record `git rev-parse HEAD`, `rustc -Vv`, SHA256 of root `Cargo.lock`, `.config/nextest.toml`, controlled font manifest and changed-source list before/after. Require clean committed candidate for Fedora. Native environment: `CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell`, `PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig`. Fedora controller environment: `CARGO_TARGET_DIR=/home/parallels/qos-target`, `CARGO_BUILD_JOBS=4`, `WGPU_BACKEND=vulkan`, `WGPU_ADAPTER_NAME=llvmpipe`, `VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json`. Native GPU receipt selects/reports Metal explicitly. Clear every VELLO override and record wgpu overrides; use the existing wrapper for each runtime command. HOME is unchanged.

Compile/lint without shell runtime (both hosts unless marked):

```sh
cargo fmt --all -- --check
cargo check --manifest-path Cargo.toml --locked -p shell -p file-explorer --lib
cargo check --manifest-path Cargo.toml --locked -p shell --example redraw_bench
cargo clippy --manifest-path Cargo.toml --locked -p shell --lib -- -D warnings
cargo clippy --manifest-path Cargo.toml --locked -p file-explorer --all-targets -- -D warnings
cargo clippy --manifest-path Cargo.toml --locked -p shell --example redraw_bench -- -D warnings
# Fedora only: preserve production target compatibility; no execution.
cargo check --manifest-path Cargo.toml --locked -p shell --all-targets
cargo clippy --manifest-path Cargo.toml --locked -p shell --all-targets -- -D warnings
```

### Native shell test-harness Clippy: required per-task evidence

Run for each task that changes shell library-test code, including Tasks 1/2 guards/tests and Task 3 retained-test conversion. File-explorer test code is covered by the required `-p file-explorer --all-targets` Clippy command above. The native shell command must select the actual Cargo `--lib` test target with dev-dependencies, then use the already proven exec-based wrapper:

`.superpowers/sdd/2026-09-13-shared-retained-gpu-target/task-2-fix1-clippy-wrapper.py`

That wrapper replaces rustc with its adjacent clippy-driver only when Cargo passes BOTH `--crate-name shell` and `--test`; it preserves jobserver descriptors with `os.execv` and appends `-D warnings`. Reuse it unchanged. Prior selection proof is documented in `task-2-report.md` under “Native all-target limitation and actual cfg(test) lint proof,” with the negative/green argv and logs there. Do NOT run `task-2-fix1-clippy-harness.py` or repeat its temporary negative source injection unless the wrapper/target-selection mechanism changes. A new task/source SHA needs a fresh positive selected-harness receipt, not another negative mutation. `cargo clippy --lib ... --test` is not valid because it forwards the flag to dependencies without the correct dev-dependencies.

The following is an implementation-time command, NOT run for this draft. Pass `task-1`, `task-2` or `task-3` for the accepted source owner being validated, and a fresh `run-1`, `run-2`, etc. label so earlier failed/cached evidence is preserved. It records the candidate commit plus all shell/file-explorer source hashes, root lock/config hashes, wrapper hash, actual driver argv, log and exit status. It does not execute tests. A cache-only Cargo success without fresh wrapper selection is failure of the lint evidence. To force the established selection without changing source bytes, the command refreshes only `shell/src/lib.rs`'s timestamp, then verifies all recorded bytes are unchanged. Run on the committed candidate under that task's sole source ownership; a later fix requires a new SHA/receipt.

```sh
python3 - task-1 run-1 <<'LINT_RECEIPT'
from pathlib import Path
import hashlib, json, os, re, subprocess, sys
fixture_task = sys.argv[1]
run_label = sys.argv[2]
if not re.fullmatch(r"run-[1-9][0-9]*", run_label):
    raise SystemExit("expected run-1, run-2, ...")
if fixture_task not in {"task-1", "task-2", "task-3"}:
    raise SystemExit("expected task-1, task-2 or task-3")
root = Path.cwd()
revision = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
subprocess.run(["git", "diff", "--quiet"], check=True)
subprocess.run(["git", "diff", "--cached", "--quiet"], check=True)
artifacts = root / ".superpowers/sdd/2026-09-13-explicit-shell-fixture-services"
wrapper = root / ".superpowers/sdd/2026-09-13-shared-retained-gpu-target/task-2-fix1-clippy-wrapper.py"
receipt_id = f"fixture-{fixture_task}-{revision[:12]}-native-test-clippy-{run_label}"
receipt_path = artifacts / f"{receipt_id}-receipt.json"
argv_path = artifacts / f"{receipt_id}-argv.json"
log_path = artifacts / f"{receipt_id}.log"
if any(path.exists() for path in [receipt_path, argv_path, log_path]):
    raise SystemExit("keep existing evidence; use a fresh receipt suffix for a rerun")
source_paths = sorted(set(root.joinpath("shell/src").rglob("*.rs"))
                    | set(root.joinpath("shell/examples").rglob("*.rs"))
                    | set(root.joinpath("apps/file-explorer/src").rglob("*.rs")))
tracked_inputs = [root / "Cargo.lock", root / ".config/nextest.toml", wrapper, *source_paths]
def hashes():
    return {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in tracked_inputs}
before = hashes()
source_root = root / "shell/src/lib.rs"
if not source_root.is_file():
    raise SystemExit("selected shell library source root changed; review target selection")
os.utime(source_root, None)
environment = dict(os.environ, RUSTC_WRAPPER=str(wrapper),
                   QOS_TASK_CLIPPY_SELECTION=str(argv_path))
command = ["cargo", "test", "--manifest-path", "Cargo.toml", "--locked",
           "-p", "shell", "--lib", "--no-run", "--verbose"]
with log_path.open("w") as log:
    result = subprocess.run(command, env=environment, stdout=log, stderr=subprocess.STDOUT)
selected = json.loads(argv_path.read_text()) if argv_path.exists() else []
selection_ok = ("--test" in selected and "--crate-name" in selected
                and selected[selected.index("--crate-name") + 1] == "shell"
                and Path(selected[0]).name == "clippy-driver"
                and selected[-2:] == ["-D", "warnings"])
after = hashes()
current_revision = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
receipt_path.write_text(json.dumps({
    "task": fixture_task, "run": run_label, "source_sha": revision, "source_sha_after": current_revision,
    "command": command, "wrapper": str(wrapper), "exit_code": result.returncode,
    "actual_selected_driver_argv": selected, "selection_ok": selection_ok,
    "input_sha256_before": before, "input_sha256_after": after,
}, indent=2))
if result.returncode != 0 or not selection_ok or before != after or revision != current_revision:
    raise SystemExit("test-harness Clippy gate failed; inspect fresh receipt/log")
LINT_RECEIPT
```

Before accepting a receipt, require the fresh `TASK2_CLIPPY_TEST_SELECTION` log marker as well as argv, exact source SHA and exit zero. The historical marker name is only a label; the new receipt identifies this fixture task. Preserve failures and cached attempts separately; do not overwrite a failed receipt with a successful rerun. No widening to unrelated native Linux helper targets, suppressing warnings, dependency changes or manual cfg(test) build counts as this gate.

Discover focused test counts from actual new binaries (both hosts). Do not invent counts, use historical counts as current, or sum overlapping expressions. Save JSON listing and exact names for each distinct target/selection, fail if any intended selection is empty or expected new names absent. Counts and platform applicability need controller review before run.

```sh
cargo nextest list --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci -p file-explorer --lib -E 'test(injected_startup_tests::)' --message-format json
cargo nextest list --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci -p shell --lib -E 'test(shell::fixture_inputs::tests::) or test(shell::hosted_inputs::tests::)' --message-format json
cargo nextest list --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci -p shell --lib -E 'test(shell::frame::retained_tests::) or test(native_4k_framebuilder_actions_match_current_staging_for_one_and_two_targets)' --message-format json
cargo nextest list --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci -p shell --example redraw_bench --message-format json
```

Task 1/2 CPU safety gate (run AFTER pre-side-effect guard/source review, before GPU runtime):

```sh
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail -p file-explorer --lib -E 'test(injected_startup_tests::)'
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail -p shell --lib -E 'test(shell::fixture_inputs::tests::) or test(shell::hosted_inputs::tests::)'
```

Task 3 CONTROLLER-OWNED GPU/runtime gate (only after Tasks 1/2 acceptance and the committed Task 3 conversion has passed independent review of every selected construction site; authors do not execute these commands):

```sh
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail --test-threads 1 -p shell --lib -E 'test(shell::frame::retained_tests::) or test(native_4k_framebuilder_actions_match_current_staging_for_one_and_two_targets)'
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --manifest-path Cargo.toml --locked --config-file .config/nextest.toml --profile ci --retries 0 --no-tests fail --test-threads 1 -p shell --example redraw_bench
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo run --manifest-path Cargo.toml --locked --release -p shell --example redraw_bench
```

No missing adapter/soft skip, retry, incomplete sample set, timeout or empty selection is success. Native shell all-target compile/lint has known unported Linux-helper failures; production-library Clippy alone is inadequate for test edits. Use the proven Cargo-selected native shell library-test wrapper below instead of broad native shell all-target Clippy or a manually appended `--test`. Native file-explorer all-target Clippy and Fedora shell/file-explorer all-target Clippy are required. Fedora all-target compile/lint remains a compatibility gate. Existing isolated visual-golden tail scheduling is preserved, but unconverted legacy goldens are NOT run by this prerequisite. A full shell nextest command is deliberately not authorized until the remaining lexical inventory has explicit input coverage; report this as an unresolved full regression prerequisite, not as a pass. The normal post-push CI notification still applies; investigate failures using `fgj actions` and keep relevant CI green before merge. Do not let CI execute unreviewed ambient constructor tests as an accidental substitute for the safety audit.

## Complete affected-file list for this candidate

Production contract/body edits: `shell/src/shell/fixture_inputs.rs` (new), `shell/src/shell/hosted_inputs.rs`, `shell/src/shell/mod.rs`, `shell/src/shell/frame.rs`, `shell/src/shell/chrome_state.rs`, `shell/src/shell/overlay_host.rs`, `shell/src/shell/qs_refresh.rs`, `shell/src/shell/qs_actions.rs`, `shell/src/shell/projection.rs`, `shell/src/shell/projection_overlay.rs`, `apps/file-explorer/src/files_app.rs`, `apps/file-explorer/src/model.rs`.

Tests/instrumentation/consumer edits: `shell/src/shell/test_support.rs`, `shell/src/shell/catalogue.rs` (test guard only), `shell/src/shell/status.rs` (test guard only), `shell/src/shell/state.rs` (test guard before ambient local identity lookup only), `shell/src/shell/search/providers/files.rs` (test guard only), `shell/src/shell/search/files_index.rs` (test guard only), `shell/src/shell/frame_retained_tests.rs`, `shell/src/shell/render/paint.rs` (test construction only), `shell/examples/redraw_bench.rs`, `shell/examples/support/populated_shell_fixture.rs` (new example-only fixture helper), `.config/nextest.toml` (only if new actual GPU tests need scheduling). Tests in existing edited files are included in that list. No `Cargo.toml`, `Cargo.lock`, UI fonts, thumbnail service or live Linux backend source changes.

Review report must show whether the complete list changed at the final source anchor, exact tested names/counts on both hosts and any build/lint failure. If the fixture action closure reaches another OS/file/network startup not in this list, stop runtime and revise this plan with the concrete source path; do not tack on a broad disable switch.

## Unresolved prerequisites / acceptance boundary

1. Active typography author's release and accepted controlled-font renderer contract; check source hashes again before edits. Font manifest is explicit data; cross-host typography pixel acceptance remains its own gate.
2. Controller approval of this bounded service-absence fixture profile and source writer assignment. The approach preserves the measured scene/action workload, but cannot honestly certify arbitrary historically ambient pixels.
3. Exact discovered test counts and applicability at implementation SHA; no builds/discovery were allowed for this draft.
4. Full shell unit/integration/golden runtime remains unsafe at old constructor sites listed in the source evidence. This small prerequisite can unblock the selected retained and populated benchmark routes without pretending to complete that larger migration. If the controller requires full shell runtime before GPU Task 2 acceptance, an explicit follow-on call-site migration/audit is necessary before those commands, not another unsafe run.
5. Files general navigation/service authority and standalone Notes/Files integration remain later work. This candidate keeps the existing real embedded widgets and synthetic fixture data; it is not the real app-host port.

Self-review performed: every construction boundary discovered above has either an explicit input/absence change, a preserved production path, or a stated excluded action/suite prerequisite. No parent runtime, source change or user-data inspection is needed to review this candidate.


## Preflight revision response — 2026-09-13

- **Files safety:** replaced the thumbnail counter-only scheme with a process-wide fail-closed deny guard before thumbnail cache/path/open, TLS opener access, all real SystemRunner launch/open methods and ambient is_workspace_path discovery. Separate owned-listing observations retain actual file reads. Every selected injected_startup_tests case must arm guards and convert constructors before any runtime; RED is compile failure or a guard panic before I/O.
- **Ownership:** switched the execution contract to SDD with fresh task implementers and independent spec/quality reviews. Tasks 1/2 end at accepted native CPU/build/lint evidence. Task 3 author converts/compiles/lints and commits; controller owns native GPU, Fedora and benchmark execution only after independent review of that committed conversion.
- **Cross-crate precision:** shell counters do not intercept normal dependency builds. The controller source-verifies all 13 Dispatcher Err implementations and the accepted explicit Files delegation before shell CPU invocation tests. File-explorer safety guards are exercised in its separate library-test gate; no cross-crate test framework was added.
- **Native test lint:** added mandatory file-explorer all-target Clippy and the existing proven Cargo-selected shell library-test wrapper, with fresh per-task/source-SHA receipt, actual driver argv and source/config hashes. Cached success is insufficient. Existing negative-control proof is reused unchanged; no repeated injected negative source unless selection changes. Fedora all-target lint stays required.

Revision performed by reading source/artifacts and editing this candidate only. No source files, builds, tests, VM/GUI/network state, git state or subagents were changed or invoked.

## Controller adoption clarifications

- Run Tasks1/2 exact-SHA focused CPU selections and affected compile/clippy gates on Fedora after native candidate/review and before accepting the task or dispatching its consumer. Task1 Linux missing-files fallback must be verified there, not inferred from Mac. No GPU work follows from either CPU gate.
- Native library-test Clippy uses the existing wrapper unchanged and a fresh positive receipt after a candidate commit. Source owner may create the scoped candidate commit before this final no-run lint, fix/amend through a new scoped commit if needed, and report all SHAs. This resolves the apparent pre-commit lint versus committed-receipt ordering; the task is not accepted until lint passes on final bytes.
- Use this plan's own SDD workspace for new briefs/reports/receipts. Historical proven wrapper/evidence remains referenced at its original path.
