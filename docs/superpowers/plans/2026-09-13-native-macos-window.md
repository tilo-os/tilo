# Native macOS Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Use one source author at a time, with controller review and exact-SHA Fedora verification after each native-tested source commit. Steps use checkbox syntax for tracking.

**Goal:** Run the shared shell workspace, chrome and real registered components in one macOS window with correct Metal presentation, Retina input, IME, accessibility and lifecycle, while preserving Linux compositor and headless modes.

**Architecture:** The existing FrameBuilder remains the retained widget/layout/scene owner. A macOS ApplicationHandler receives native callbacks on the main thread and consumes the accepted RetainedGpuTarget on the surface-compatible device. A small shared shell host coordinates already trusted in-process component admission and services; neither a second shell widget tree nor a new process protocol is introduced.

**Tech Stack:** Current Rust workspace and lockfile; vendored winit; Vello/wgpu 29; ui::gpu::RenderContext; AccessKit winit 0.34.0 already used by ui; common ApplicationDefinition/HostContext/AppComponent and runtime-fs provider candidates.

**Spec:** docs/superpowers/specs/2026-09-12-macos-shell-port-design.md; docs/superpowers/specs/2026-09-12-macos-app-capability-map.md.

**Planning producers:** docs/superpowers/plans/2026-09-13-shared-retained-gpu-target.md; 2026-09-13-common-real-app-framework.md; 2026-09-13-real-notes-component.md Tasks 1–3; 2026-09-13-real-files-provider.md Tasks 1–6.

**Status:** Revised after five controller preflight findings, 2026-09-13. Source inspected at 4898888d7ea96807b3fb4c041fd283b75ec4c2c1; direct-core chooser/lint corrections do not move these owners. All new signatures below are proposed contracts, not existing implemented APIs. No source implementation, build, test, GUI, VM or commit was performed to author this plan. Controller adopted this plan after reconciling the shared dispatcher API and the Files service-owner/library-test dependency below. This document records planned work, not implementation evidence.

## Global Constraints

- “Reuse WorkspaceTree, shell chrome, actions and FrameBuilder as the scene producer.”
- “On macOS the shell is one resizable, optionally fullscreen application window containing qOS app tiles, chrome and workspaces.”
- “On Linux it retains its current native compositor, nested and headless modes.”
- “The first milestone is **real Notes and Files, with a complete capability map for every other app**.”
- “Apps and ui must not depend on shell to request a service.”
- “Benchmarks and tests must not load the user's real state or secrets.”
- Apple Silicon first; macOS 13 is a proposed deployment minimum requiring physical-machine/toolchain qualification, not a result of these tests.
- Same source/lockfile, target cfg for operating systems, no new OS-selection feature or dependency upgrade. Keep Linux defaults, HDR and its render thread.
- No normal framebuffer readback, framebuffer staging or XRGB conversion on Mac; tiny Vello overflow counters remain required GPU metadata.
- Keep the accepted Linux one-second fence, fresh one-second framebuffer callback and one-second counter-settlement phases per attempt, at most four attempts. Never turn these into a one-second whole-frame deadline or relax them for a pass.
- GPU-only render_until consumes one caller absolute deadline across all attempts. Native surface work does not reset it.
- Explicit fixture modes establish adapter behavior only. The actual Notes/Files definitions and authenticated, real services qualify application behavior.
- Mac login, system lock, Spaces and arbitrary host windows remain host-owned. This runner neither starts a Wayland server nor emulates Linux machine first boot.
- No dependency on the absent authenticated-component-session or standalone SDK bootstrap. Those concern separate processes. Native trusted in-process admission is Task 6 here and consumes existing shared authority plus Files Task 6.

---

## Source evidence and decisions

| Inspected source | Implication |
| --- | --- |
| shell/src/main.rs:118,215; compositor/mod.rs target gates | Current binary startup imports Linux-gated hosted/native modules and performs firstboot/portal/render-node work. Split Linux orchestration from the tiny target entrypoint; do not call it on Mac. |
| vendor/winit/src/event_loop.rs:83; platform/pump_events.rs:30–68 | Create EventLoop on main, use run_app, process RedrawRequested and lifecycle synchronously. The Linux pumping backend cannot become the Mac runner. |
| shell/src/shell/frame.rs:292,375,433,460,725,993 | FrameBuilder already retains host/chrome/Taffy/widget pointers and scene revisions. Its real-session constructors start StatusBridge; private from_parts reads shots overrides. Add an explicit prepared-host constructor rather than inheriting those side effects. |
| frame.rs:1197,2081,2101; overlay_host.rs:7256,11144 | Current viewport and damage quantization assume scale 1; dispatch hardcodes 1.0. Widgets already divide physical pointer/pixel-scroll coordinates by their supplied scale. Pass raw input and the actual scale, rather than preconverting some events. |
| frame.rs:1964,1976; ui/src/app.rs:4560 | FrameBuilder has a11y projection/action entrypoints, including scale-1 synthetic click fallback. App adds resources. Make FrameBuilder consume bound-resource semantics and retain its internal logical synthetic route. |
| ui/src/app.rs:4300,6153,7711; Widget::ime_cursor_rect/purpose | Reuse focused widget IME purpose/caret; caret is window-logical. Create AccessKit adapter before showing the window. Apply purpose, enable, then logical caret geometry. |
| ui/src/gpu.rs:31,82,190–274 | RenderContext owns adapter/device/queue, selects against surface, but create_surface allocates its own intermediate and configures unchecked. Add a surface-only seam; do not allocate a redundant target beside RetainedGpuTarget. |
| ui/src/render_thread.rs:304–348,643–755 | Reuse narrow configure validation handling and TextureBlitter. Pinned wgpu uses CurrentSurfaceTexture::{Success,Suboptimal,Outdated,Occluded,Timeout,Lost,Validation}, not the old Result/SurfaceError API. Do not reuse its separate rasterizer. |
| shell/src/shell/present.rs | Linux permits one queued frame beyond one presenting; retirement is serial-specific; shutdown grace is two seconds. Native starts with zero queued CPU scenes and one callback transaction, within that bound. Keep Linux implementation untouched. |
| ui/src/wake.rs | WakeScope binds once, retains prebind wakes, checks lifetime, closes before teardown. Bind proxy-only sinks and reject late child wakes. |
| shell/src/identity_registry.rs:223; device_fleet.rs:66 | IdentityRegistryStore::at and DeviceFleetStore::at accept explicit roots. Use their existing formats and selection; do not build a parallel identity registry. |
| shell/src/shell/network.rs:718,3292; firstboot.rs:851 | Network spawn currently load-or-creates a key and derives ambient identity. A strict explicit enrolled-host constructor must be added for native use; no invalid-certificate fallback. Existing verification is reusable but assumes primary/single-device; validate selected workspace certificates explicitly as well. |
| shell/src/shell/hosted_inputs.rs; mod.rs:1317 | Explicit fixture ownership and missing-service errors exist. Preserve fixtures. The prepared real-component path must use the Files/common framework producers rather than these legacy lightweight constructors. |
| shell/src/shell/catalogue.rs | This is remote package catalogue metadata backed by qos-catalogue, not the linked ApplicationCatalog. Do not replace it or interpret installed packages as Mac-loadable components. |

## Delivery boundaries and preflight

Tasks 1–5 produce a working native adapter and an explicitly isolated window probe. Task 6 connects a strict, configured, enrolled in-process host and the same actual linked app definitions. Task 7 qualifies the real native slice and preserves Fedora behavior. Building the probe alone is not task-chain completion.

Dependency order is exactly Tasks 1 → 2 → 3 → 4 → 5 → 6 → 7. Task 4 defines the platform-neutral lifecycle owner; Task 5 defines PreparedShellHost, its explicit probe constructor and retained-close recovery; Task 6 adds only production configuration/admission and main. The early Mac deliverable is a real explicit example, not a stub successful main. Until Task 6 ports main and classifies existing targets, Mac gates use library modules and the explicit probe example; Fedora continues all-target gates. Cargo integration tests can build package binaries, so early tests must not use shell --test it on Mac.

| Contract | Producer and exact consumer | Implementation dependency / preflight |
| --- | --- | --- |
| Completed retained texture | GPU plan: RetainedGpuTarget::new(&DeviceHandle,u32,u32), render_until(&Scene,SceneDamage,Instant), completed_view(), resize(), DeviceHandle::loss_monitor() | Required before Task 4. No change to its accepted signature or Linux timeout policy. |
| Canonical factory/lifetime | Common framework: ApplicationCatalog, HostContext::prepare, ApplicationDefinition::instantiate, AppComponent::mount/close, TileHost::from_component | Tasks 2 and 6 consume it. Shell must be able to use its bound-node helper; exact public extension is fixed below. |
| Actual Notes | notes::definition() from Notes Tasks 1–3 | Task 6 links it. Notes Task 4/standalone process bootstrap is not a prerequisite. |
| Actual Files | file_explorer::application_definition(); FilesContext and real provider Tasks 1–6 | Task 6 consumes their shared host owner. Files Task 7 standalone sub-gate is separate. |
| Files durable service ownership | NativeGrantAuthority, FsSession, OperationOwner, AppLocalStateOwner, FileTransfers, RecordIndexClient/OpenTargetClient | Reuse Files plan types, quotas, mount UUID/lifetime, recovery and transfer policy. Do not define duplicate providers in this plan. |
| Native enrolled host | Task 6 NativeHostConfig → PreparedShellHost | New in-process preparation, using explicit existing identity/profile/device roots and live workspace authority. No process credential or issuer service required. |
| Controlled typography | Existing typography renderer/fixture qualification producers | Functional adapter tests can run independently; cross-host pixel comparison requires the accepted controlled font injection and records it. |
| Device-native acquisition | Keychain/first-user enrollment; native chooser/security-scoped URLs; Mac Trash and cross-volume checks | Separate qualified provider work. Startup grants and an enrolled test profile allow real in-process apps now; missing providers remain visible and cannot count as first-milestone completion. |
| Actual collaboration | Strict enrolled network constructor here plus accepted shared record/presence/authority transport | Local shell/real-file tests do not prove Mac↔Fedora collaboration. Record-owner delta and instance presence bindings must exist and be tested before that claim. |

Controller ratified the common bound-node extension in principle. Task 2 now provides the exact producer declaration for direct adoption into common-framework Task 3; shell consumes that one implementation. Files Task 6 produces FilesServiceOwner; native Task 5 imports it into PreparedShellHost, and native Task 6 configures production admission. These are in-process integration seams, not a new service architecture or dependency on the standalone-session predecessor.

## File ownership

| File | Responsibility |
| --- | --- |
| shell/src/main.rs; new shell/src/linux_main.rs | Tiny target dispatch; existing Linux orchestration moved without behavioral rewrite. |
| new shell/src/native_window/{mod,presentation}.rs | Mac-gated main-thread callbacks and retained-target presentation; callbacks use the shared WindowLifecycle owner below. |
| new shell/src/shell/window_lifecycle.rs | Platform-neutral WindowLifecycle actually owned by the Mac ApplicationHandler; geometry/occlusion/retry/serial/close state and admission decisions, with colocated library tests. |
| shell/src/shell/frame.rs; new shell/src/shell/frame_viewport.rs | Same retained producer with explicit logical layout/physical scene mapping; existing compatibility wrappers. |
| ui/src/{gpu,app,lib}.rs; new ui/src/ime.rs | Surface-only GPU resource, shared finite logical IME state. Preserve ui worker and old GPU APIs. |
| ui/src/bound_dispatch.rs; shell/src/shell/frame.rs | Existing component-bound callbacks made usable by FrameBuilder's raw pointer paths. |
| new shell/src/shell/native_host.rs | Task 5 shared PreparedShellHost and retained close recovery, consuming Files Task 6 FilesServiceOwner; Task 6 adds strict preparation, no RPC protocol. |
| shell/src/shell/{hosted_inputs,mod,chrome_state,overlay_host,doc_open,placement}.rs | Consume Files Task 6 component host fields, route real catalogue opens and lifecycle through existing shell. |
| shell/src/shell/network.rs; shell/src/network/actor.rs; identity_registry.rs/device_fleet.rs only if required for explicit refresh | Strict enrolled inputs and nonambient identity refresh. Preserve Linux compatibility constructors. |
| shell/Cargo.toml; shell/src/lib.rs; shell/src/shell/mod.rs; shell/tests/it.rs; .config/nextest.toml | Early library-module registration; Task 6 classifies true Linux targets before Mac all-target gates. Preserve GPU scheduling and portable test execution. |
| new shell/src/shell/{native_viewport_tests,native_component_dispatch_tests,native_host_tests}.rs | Geometry, actual FrameBuilder binding and retained close/admission regressions, registered via cfg(test) under the shared shell module. No early integration-target dependency on main. |
| new shell/examples/native_window_probe.rs | Explicit isolated adapter probe with absolute scratch directory and bounded run option; never a production app substitute. |
| shell/src/shell/real_files_host_tests.rs from Files Task 6; capability map; this plan's SDD evidence ledger | Real factories/native integration and exact-SHA results, separate adapter/app/provider claims. |

## Task 1: Give the retained producer an exact output geometry contract

**Consumes:** Existing FrameBuilder/FramePaint; physical winit size and scale.
**Produces:** FrameViewport and explicit construction/geometry APIs below. Existing FrameBuilder methods and Linux re-exports remain usable.

~~~rust
// shell::shell::frame_viewport
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameViewport { physical: (u32, u32), scale: f64 }
#[derive(Debug, thiserror::Error)]
pub enum FrameGeometryError {
    #[error("output scale must be finite and positive")] InvalidScale,
    #[error("logical viewport exceeds the layout range")] InvalidExtent,
}
impl FrameViewport {
    pub fn new(physical: (u32, u32), scale: f64)
        -> Result<Option<Self>, FrameGeometryError>;
    pub fn physical(self) -> (u32, u32);
    pub fn scale(self) -> f64;
    pub fn logical(self) -> (f64, f64);
    pub fn damage(self, logical: vello::kurbo::Rect)
        -> Option<vello::FineDamageRect>;
}
impl FrameBuilder {
    pub fn from_host(
        parts: (ShellOverlayHost, Chrome), viewport: FrameViewport,
        theme: ui::theme::Theme, text: ui::text_render::TextRenderer,
    ) -> Self;
    pub fn set_frame_viewport(&mut self, viewport: FrameViewport) -> bool;
    pub fn dispatch_event_scaled(&mut self, event: &winit::event::WindowEvent,
        scale: f64, modifiers: &winit::event::Modifiers,
        state: &mut automerge::AutoCommit, root: &automerge::ObjId,
        focus: &mut Option<taffy::NodeId>) -> bool;
}
~~~

- [ ] Add tests directly against FrameViewport, including a fractional logical right/bottom edge, negative/offscreen damage, nonfinite scale/rect, zero extent, very large extent and 1×/2× equivalents.

Register them in the shared library, not shell/tests/it.rs:

~~~rust
// shell/src/shell/mod.rs
pub mod frame_viewport;
#[cfg(test)]
#[path = "native_viewport_tests.rs"]
mod native_viewport_tests;
~~~

Task 2 registers native_component_dispatch_tests the same way; Task 5 registers native_host_tests. Task 4's window_lifecycle.rs has its own cfg(test) mod tests. These modules are not target_os gated and run on both hosts.

~~~rust
let viewport = FrameViewport::new((801, 601), 2.0).unwrap().unwrap();
assert_eq!(viewport.logical(), (400.5, 300.5));
let edge = viewport.damage(vello::kurbo::Rect::new(400.0, 300.0, 400.5, 300.5)).unwrap();
assert_eq!((edge.x, edge.y, edge.width, edge.height), (800, 600, 1, 1));
assert!(FrameViewport::new((0, 601), 2.0).unwrap().is_none());
assert!(FrameViewport::new((801, 601), f64::NAN).is_err());
~~~

- [ ] Run the new shell native_viewport selection and observe failure before adding the implementation.
- [ ] Store output viewport in FrameBuilder with scale 1 for legacy constructors. Reject nonfinite/nonpositive scale and logical dimensions not representable as finite f32 layout values. Zero physical dimensions return None: a runner retains its last valid layout and does no allocation/acquire/render until nonzero.
- [ ] Perform layout in physical/scale logical f64, narrowing only at Taffy. Update wrapper style/available size when geometry changes, preserving actual widget boxes. Equal geometry is a no-op. Retain current structural dirty/focus reconciliation for actual structure changes.
- [ ] Calculate damage from the original logical Rect inside native_damage_rect, multiplying by scale BEFORE floor/ceil/clamp. Reject nonfinite coordinates; floor min, ceil max, clip against exact physical extent, then narrow to u32. Do not first round to integer logical pixels: current fractional viewport edge would lose pixels.
- [ ] Keep produced section comparisons in logical scene space. Append the final composed logical scene once using Affine::scale(scale) into the returned FramePaint scene. FramePaint.damage_regions thereafter always means physical target pixels. Geometry changes clear last_produced_sections so the next paint is dense. Scale-1 existing snapshots and sparse admission remain unchanged.
- [ ] Implement dispatch_event_scaled by passing raw WindowEvent and actual scale to existing host.handle_event. Existing dispatch_event remains the exact scale-1 wrapper used by Linux/synthetic a11y/shots. Reject invalid scales. Keep line scroll units unchanged; existing widget pixel scroll and touch paths do their one conversion. Test pointer hit and pixel scroll equivalence at 1× and 2× on actual FrameBuilder widgets.
- [ ] from_host consumes prepared parts and explicit theme/text renderer without StatusBridge, environment reads, factory recreation or default fixture setup. Legacy constructors retain their old options. Propagate PlatformConventions through the existing Widget hook before layout.
- [ ] Add actual FrameBuilder sparse/dense target comparisons for logical damage at 1, 1.25 and 2 scales, right/bottom clipping, unchanged logical size with changed physical extent, and scale-only change. Require no sparse reuse across geometry changes. Use controlled font source for cross-host comparisons; scene assertions remain available before that producer lands.
- [ ] Run Task 1 gates below and commit: feat(shell): add logical viewport and physical retained damage.

## Task 2: Bind FrameBuilder's direct callbacks and expose shared IME state

**Consumes:** Common framework BoundNodeMap/CallbackBinding/TileHost binding, existing App focused-widget IME behavior.
**Produces:** Bound FrameBuilder traversal plus these shared contracts; use common implementation, not a second binding engine.

~~~rust
// Exact producer declarations in ui/src/bound_dispatch.rs, common Task 3.
// Add `pub mod bound_dispatch` in ui/src/lib.rs; shell imports these, never copies them.
// Private fields remain producer-owned.
impl BoundNodeMap {
    pub fn new() -> Self;
    pub fn rebuild(&mut self, root: &dyn crate::widget::Widget,
        nodes: &std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>);
    pub fn clear(&mut self);
}
#[derive(Debug)]
pub enum BoundDispatchError {
    MissingNode,
    ClosedLifetime,
    Record(crate::record::RecordOpenError),
}
// Safety: caller owns the live widget tree matching nodes and bindings; no
// replacement/rebuild may occur while callback runs. Shared entry reuses an
// already-active same-record borrow, exactly as required by framework Task 3.
pub unsafe fn invoke_bound_node<R>(
    bindings: &BoundNodeMap,
    nodes: &std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    node: taffy::NodeId, outer: &mut automerge::AutoCommit,
    outer_root: &automerge::ObjId,
    callback: impl FnOnce(&mut dyn crate::widget::Widget,
        &mut automerge::AutoCommit, &automerge::ObjId) -> R,
) -> Result<R, BoundDispatchError>;

// ui::ime; this is a value snapshot, contains no platform window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImeState {
    pub purpose: winit::window::ImePurpose,
    pub cursor: vello::kurbo::Rect, // finite window-logical geometry
}
impl ImeState {
    pub fn from_widget(widget: &dyn ui::widget::Widget) -> Option<Self>;
    pub fn apply_to_window(next: Option<Self>, window: &winit::window::Window);
}
impl FrameBuilder {
    pub fn ime_state(&mut self, focus: Option<taffy::NodeId>,
        outer: &mut automerge::AutoCommit, root: &automerge::ObjId) -> Option<ui::ime::ImeState>;
    pub fn cursor_at(&mut self, logical: vello::kurbo::Point,
        outer: &mut automerge::AutoCommit, root: &automerge::ObjId)
        -> winit::window::CursorIcon;
    pub fn clear_native_focus(&mut self, focus: &mut Option<taffy::NodeId>,
        outer: &mut automerge::AutoCommit, root: &automerge::ObjId);
}
~~~

The BoundNodeMap block is the exact public producer signature, using crate::widget because its sole implementation lives in ui. new() creates its empty private maps; keep Default if already present. Shell consumes ui::bound_dispatch::{BoundNodeMap, BoundDispatchError, invoke_bound_node}. Record retains the existing typed RecordOpenError. No alternate names or compatibility binding engine are authorized by this plan.

The unsafe call requires exclusive live ownership of the matching widget tree, current node map and binding generation for the entire callback. Clear before any replacement or destruction; neither references nor raw callbacks escape the call. A requested component binding that is closed/missing a canonical record returns an error before invoking callback; it never falls back to outer. Unbound shell widgets retain their explicit outer document. Same-record reentry uses the common already-entered dispatch context/document and must not reacquire RefCell mutably. Synthetic a11y/resource nodes without a Taffy node retain the originating component root's CallbackBinding through the existing common guarded semantic/resource traversal; this raw-node helper does not reconstruct origin from a NodeId or route them as unbound shell work. Include that case in the producer and shell conformance tests.

- [ ] Register native_component_dispatch_tests under shell/src/shell/mod.rs using cfg(test) and create the explicit probe there from the producer's documented ApplicationDefinition contract (integration-test support is not a public dependency). Mount it inside actual FrameBuilder. Count factory, measure, focus, IME, semantic/resource action and close calls; two instances own distinct records/WakeScopes. Invoke real raw descendants and synthetic resource origin paths, not just root.handle_event.
- [ ] Assert an IME/measure/focus callback captures the child's route; a direct semantic action changes only the child's canonical record; the outer shell record stays unchanged; a closed/reopened durable tile rejects its old binding/wake; an ordinary input event does not increment factory count. Rebuild once to prove old NodeIds cannot be dispatched.
- [ ] Run the selection and observe the existing unbound direct-path failure.
- [ ] Build/clear BoundNodeMap alongside FrameBuilder's existing node_map; wrap every raw dereference in measure, focus order, set_focus, a11y dispatch, cursor and IME. Callbacks without document effects still enter the bound lifetime. Retained tree walks use the framework's guarded root; do not reborrow the same record recursively. Missing/closed binding skips delivery and never falls back to outer document after component binding was requested.
- [ ] Carry resource semantics and actions from common framework Task 4 into FrameBuilder a11y projection and shell palette/control dispatch. Reuse the shared resource invocation door and action transport; do not route a component action through the outer shell UndoLog. Add action-via-keyboard/palette/a11y parity with denial-after-discovery and independent undo. Preserve existing Linux compositor a11y publisher.
- [ ] Extract App's private IME snapshot/application into ui::ime. Keep fractional finite caret coordinates. Normalize negative width/height by returning None; retain finite negative origin for offscreen caret rather than wrapping to unsigned. Enforce minimum logical width/height of 1 for native candidate placement and clip only if the shared widget's contract requires it. Apply purpose before enabled=true, then LogicalPosition/LogicalSize. None disables input. Existing App uses the same helper.
- [ ] Test purpose/caret snapshot from real TextInput plus non-text focus, empty/preedit state and fractional geometry; preserve existing UI IME tests. Before focus changes or removal, deliver the existing widget IME cancellation/Disabled path under the old binding, then clear host native composition state; never commit preedit text to a newly focused tile.
- [ ] cursor_at follows App's smallest-area live cursor claim, using logical pointer position and bound callbacks. clear_native_focus invalidates IME/focus state without destroying the component; returning focus can use the retained valid focus target after layout reconciliation.
- [ ] Run Task 2 gates, then commit: fix(shell): bind frame callbacks and share native ime state.

## Task 3: Separate surface configuration from the renderer's retained target

**Consumes:** ui::gpu device selection/loss monitor and pinned TextureBlitter.
**Produces:** A surface-only owner in ui::gpu, with old RenderSurface APIs preserved.

~~~rust
pub struct WindowSurface<'w> {
    pub surface: vello::wgpu::Surface<'w>,
    pub config: vello::wgpu::SurfaceConfiguration,
    pub dev_id: usize,
    pub blitter: vello::wgpu::util::TextureBlitter,
}
#[derive(Debug, thiserror::Error)]
pub enum WindowSurfaceError {
    #[error("{0}")] Create(#[from] CreateSurfaceError),
    #[error("surface configuration failed: {0}")] Configure(String),
    #[error("surface device is missing")] MissingDevice,
    #[error("zero extent")] ZeroExtent,
}
impl RenderContext {
    pub fn with_backends(backends: vello::wgpu::Backends) -> Self;
    pub async fn create_window_surface<'w>(
        &mut self, window: impl Into<vello::wgpu::SurfaceTarget<'w>>,
        size: (u32, u32), present_mode: vello::wgpu::PresentMode,
    ) -> Result<WindowSurface<'w>, WindowSurfaceError>;
    pub async fn configure_window_surface(
        &self, surface: &mut WindowSurface<'_>, size: (u32,u32),
    ) -> Result<(), WindowSurfaceError>;
}
~~~

- [ ] Add unit tests for shared SDR surface-config selection against supplied capabilities: supported RGBA/BGRA unorm, no supported format, zero extent, stable format after resize, desired_maximum_frame_latency=1 for the new native path, Fifo mode, opaque alpha when supported otherwise the supported Auto equivalent. Do not claim HDR support.
- [ ] Run the GPU selection tests and observe failure.
- [ ] Extract common surface capability selection/device lookup; create_window_surface selects against that actual surface and allocates no intermediate target. Reuse device settings/loss observation from RenderContext. The Mac runner uses with_backends(Backends::METAL); legacy new retains existing environment behavior. Do not allow a software/other backend result to be reported as Metal evidence.
- [ ] Configure with a validation error scope around only Surface::configure. Catch the result and retain the previous valid configuration on failure. The asynchronous scope is completed at initialization/reconfiguration; no unbounded device wait is introduced into normal frame rendering. Device loss is checked before/after. Document that backend configure/acquire calls themselves are not preemptible by a Rust deadline.
- [ ] Retain create_surface/resize_surface's signatures and targets for ui's current worker. Share format/configuration mechanics where safe but do not move that worker onto RetainedGpuTarget or alter Linux present scheduling here.
- [ ] Verify a real Mac surface allocates only one RetainedGpuTarget later in Task 4; headless offscreen tests can inspect new target-free configuration but do not count as real surface evidence. Preserve current worker resize/lost-surface regressions on both hosts.
- [ ] Run Task 3 gates; commit: refactor(ui): expose a surface-only gpu presentation target.

## Task 4: Present the accepted GPU target inside the native redraw callback

**Consumes:** Tasks 1/3 and GPU-plan RetainedGpuTarget.
**Produces:** NativePresenter and explicit transaction outcomes.

First create the actual cross-platform lifecycle owner in shell/src/shell/window_lifecycle.rs, exported unconditionally by shell/src/shell/mod.rs. The Mac ApplicationHandler contains this exact type; it is the only owner of presentation admission, serials, retry counters and close phase. It contains no window, AccessKit or Metal objects.

~~~rust
// shell::shell::window_lifecycle
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClosePhase { Open, Closing, RecoveryRequired, ReadyToExit }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationEvent {
    Presented { serial: u64 }, Timeout, Occluded, Reconfigure,
    RecreateSurface, RecreateDevice, Deadline, Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recreate { Surface, Device }
pub struct FrameAdmission { pub serial: u64, pub deadline: std::time::Instant }
pub struct WindowDecision {
    pub request_redraw: bool,
    pub invalidate_history: bool,
    pub recreate: Option<Recreate>,
    pub wake_at: Option<std::time::Instant>,
    pub exit: bool,
}
pub struct WindowLifecycle {
    // Private viewport/visibility/suspension/dirty/in-flight serial/retry/close state.
}
impl WindowLifecycle {
    pub fn new() -> Self;
    pub fn geometry(&mut self, viewport: Option<FrameViewport>);
    pub fn resumed(&mut self);
    pub fn suspended(&mut self);
    pub fn occluded(&mut self, value: bool);
    pub fn mark_dirty(&mut self);
    pub fn close_phase(&mut self, phase: ClosePhase);
    pub fn retry_presentation(&mut self);
    pub fn begin_frame(&mut self, now: std::time::Instant) -> Option<FrameAdmission>;
    pub fn finish_frame(&mut self, outcome: PresentationEvent, now: std::time::Instant);
    pub fn take_decision(&mut self, now: std::time::Instant) -> WindowDecision;
}
~~~

NativePresenter maps every NativePresentOutcome to the matching PresentationEvent and the callback immediately feeds finish_frame. begin_frame is called before building FramePaint; if it returns None no scene is built or submitted. Its admitted deadline is the one passed through render_until. take_decision consumes transient effects once and calculates the next wake from the actual retry/dirty state. The Mac callback must not keep duplicate retry/serial/visibility counters. This makes Linux tests exercise the production owner, not a copied expected-decision model. RecoveryRequired still admits frames for trusted recovery chrome; it does not admit closed app callbacks.

~~~rust
let mut lifecycle = WindowLifecycle::new();
lifecycle.resumed();
lifecycle.geometry(FrameViewport::new((800, 600), 2.0).unwrap());
lifecycle.mark_dirty();
let now = std::time::Instant::now();
let admitted = lifecycle.begin_frame(now).unwrap();
assert!(lifecycle.begin_frame(now).is_none());
lifecycle.finish_frame(PresentationEvent::Presented { serial: admitted.serial }, now);
lifecycle.close_phase(ClosePhase::RecoveryRequired);
assert!(!lifecycle.take_decision(now).exit);
lifecycle.close_phase(ClosePhase::ReadyToExit);
assert!(lifecycle.take_decision(now).exit);
~~~

Register these as window_lifecycle::tests in the shared library. Add delayed-timeout/recreate budget, stale/wrong serial, zero extent, duplicate resume and occlusion cases against this same owner, with passed Instants rather than sleeps. Real Metal/blitter tests live under Mac-gated native_window::presentation::tests and execute separately on Mac.

~~~rust
// shell::native_window::presentation
pub struct NativePresenter {
    // Private RenderContext, WindowSurface<'static>, RetainedGpuTarget,
    // loss monitor and generation. Surface owns Arc<Window> as its target.
}
pub enum NativePresentError {
    Surface(WindowSurfaceError),
    Target(BridgeError),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameSkip { ZeroSized, Occluded, Timeout, Deadline }
pub enum NativePresentOutcome {
    Presented { serial: u64, revision: u64, stats: GpuFrameStats },
    Skipped(FrameSkip),
    Reconfigure,
    RecreateSurface,
    RecreateDevice,
    Failed(String),
}
impl NativePresenter {
    pub fn new(window: std::sync::Arc<winit::window::Window>,
        viewport: FrameViewport) -> Result<Self, NativePresentError>;
    pub fn resize(&mut self, viewport: FrameViewport) -> Result<(), NativePresentError>;
    pub fn present(&mut self, frame: &FramePaint, serial: u64,
        deadline: std::time::Instant) -> NativePresentOutcome;
}
~~~

NativePresenter is Mac-only. GPU target errors are mapped by meaning: DeviceLost requests a new device; poisoned/RecreateRequired target cannot expose a view; overflow exhaustion/validation failures remain visible failures rather than empty successes. NativePresentError preserves surface and target initialization/resize failures; runtime retained errors remain diagnosable in outcomes/logs.

- [ ] Add window_lifecycle::tests against the owner above and Mac-native presentation::tests using a narrow acquisition seam around the actual transaction: Success renders/blits/presents once, Outdated/Suboptimal does not acknowledge, Occluded/Timeout submits no Vello work, deadline submits no blit, lost device never publishes old pixels. Driver injection covers rare outcomes; actual rendering tests use actual RetainedGpuTarget.
- [ ] Run the selection before implementation.
- [ ] Create RenderContext(METAL) and WindowSurface using one Arc<Window>. Select its DeviceHandle and construct RetainedGpuTarget using that same handle; no shared headless adapter/context and no extra surface intermediate. Surface blitter samples target.completed_view, never a clone/readback of scene pixels.
- [ ] WindowLifecycle::begin_frame computes one absolute deadline as supplied now + one second using checked_add. This is the normal native explicit-wait ceiling, not a one-second wall-clock guarantee and not a change to Linux. Pass that admitted deadline through render_until; check it before render, after acquire, before blit and before present. Encoding/driver calls cannot be preempted; report overruns honestly.
- [ ] Acquire before Vello work. CurrentSurfaceTexture::Success supplies the image; Suboptimal image is dropped before reconfigure; Outdated/Suboptimal schedule reconfiguration, Occluded parks, Timeout asks for a bounded delayed retry, Lost requests surface recreation, Validation returns a visible failure. Do not turn programmer validation failures into endless reconstruction.
- [ ] Render only once for the current FramePaint using its revision/previous/damage. A failed/expired target cannot blit an earlier completed_view. On success GPU-blit the ENTIRE valid target into the acquired surface texture: each swapchain image is fresh regardless of sparse rendering. Submit the blit to the same ordered queue; call window.pre_present_notify(), then surface_texture.present() inside RedrawRequested. No CPU framebuffer staging/copy/map/swizzle is allocated or executed.
- [ ] Track submitted serial, last successfully presented serial, outcome and target revision separately. Presented means present() was called after a submitted valid blit; it does not prove physical scanout. Failed/skipped/reconfigured frames retire the callback transaction but cannot satisfy a successful-present receipt. GPU target completion is not display presentation.
- [ ] Native has zero queued scenes and at most one active callback frame. wgpu native surface latency is one. This is within the shared Linux admission bound without moving a non-Send FrameBuilder onto PresentPipe. Leave Linux PresentPipe and its lock-frame serial semantics unchanged.
- [ ] Resize discards acquired images, reconfigures nonzero extent, resizes target, and invalidates history; scale changes reset retained producer/target even if physical size unchanged. Surface format change rebuilds blitter and target validity. Device loss drops target/surface/context together and creates a new surface-compatible device. Every recovery forces the next producer frame dense.
- [ ] Add real Metal target-to-blitter-oracle tests with an offscreen destination texture and out-of-timing readback only. Assert full destination pixels match dense rendering after sparse change; initial/sparse/resize/revision-gap/error paths report zero framebuffer staging/copy. Native real window test verifies actual acquire/present separately.
- [ ] Run Task 4 gates and commit: feat(shell): present retained gpu frames on a native mac surface.

## Task 5: Own the native window, input and lifecycle on main

**Consumes:** Bound FrameBuilder, shared IME, NativePresenter, WakeScope.
**Produces:** Main-thread runner and the shared PreparedShellHost container in native_host.rs. This task owns its explicit fixture constructor and close/retry implementation; Task 6 adds production preparation without changing that ownership. Files Tasks 1–6 must already be accepted: Task 6 produces the concrete FilesServiceOwner in shell/src/shell/hosted_inputs.rs and verifies it through library tests without a native main. Import that one existing owner; never redeclare its fields or create a second service owner. The optional field is None only for an explicitly service-free probe; real Files admission requires it.

~~~rust
// shell::shell::native_host, created in Task 5, configured by Task 6.
use super::hosted_inputs::FilesServiceOwner; // produced by Files Task 6
pub struct PreparedShellHost {
    // Private FrameBuilder, outer State/focus/modifiers, open shell WakeScope;
    // the FrameBuilder tree owns the common TileHost/AppComponent mounts.
    // One canonical RecordOwners registry and existing scoped View stores;
    // optional profile lock, exactly one Option<FilesServiceOwner>;
    // retained ComponentCloseReport list and HostCloseStatus, independent of removed widgets.
}
#[derive(Clone, Debug)]
pub enum HostCloseIssueKind { View, Record, FileOperation, AppLocalState, Teardown }
#[derive(Clone, Debug)]
pub struct HostCloseIssue { pub id: u64, pub kind: HostCloseIssueKind, pub message: String }
#[derive(Clone, Debug)]
pub struct HostCloseStatus { pub pending: usize, pub issues: Vec<HostCloseIssue> }
#[derive(Clone, Debug)]
pub enum HostCloseOutcome {
    Pending(HostCloseStatus),
    RecoveryRequired(HostCloseStatus),
    ReadyToExit,
}
impl PreparedShellHost {
    pub fn with_frame<R>(&mut self, callback: impl FnOnce(
        &mut FrameBuilder, &mut ui::State, &mut winit::event::Modifiers,
        &mut Option<taffy::NodeId>) -> R) -> R;
    pub fn bind_wakes(&mut self,
        sink: impl Fn(ui::wake::WakeEvent) + Send + Sync + 'static)
        -> Result<(), ui::wake::WakeBindError>;
    pub fn accepts_wake(&self, event: &ui::wake::WakeEvent) -> bool;
    pub fn request_close(&mut self) -> HostCloseOutcome;
    pub fn retry_close(&mut self) -> HostCloseOutcome;
    pub fn poll_close(&mut self) -> HostCloseOutcome;
    pub fn pump_host_controls(&mut self) -> Option<HostCloseOutcome>;
    // Existing explicit hosted-test-control capability only; never a production fallback.
    pub fn fixture(scratch: std::path::PathBuf, viewport: FrameViewport)
        -> Result<Self, std::io::Error>;
}
~~~

with_frame enters the still-open shell WakeScope and lends disjoint fields; ordinary component boundaries use their own live bindings. After request_close, it permits trusted recovery chrome callbacks only: all component routes are closed and their node metadata detached. Status DTOs contain no ownership; PreparedShellHost retains the actual report/RecordStore/ViewStore/FilesServiceOwner handles until ReadyToExit. The fixture constructor predates NativeHostConfig and has no Task 6 dependency. Use the existing FilesServiceOwner fields and actual runtime module exports from Files Task 6; no runtime provider is implemented by this container task.

~~~rust
pub struct NativeWindowOptions {
    pub title: String,
    pub initial_logical_size: (f64, f64),
    pub fullscreen: bool,
}
pub enum NativeUserEvent {
    Wake(ui::wake::WakeEvent),
    AccessKit(accesskit_winit::WindowEvent),
}
impl From<accesskit_winit::Event> for NativeUserEvent {
    fn from(event: accesskit_winit::Event) -> Self { Self::AccessKit(event.window_event) }
}
#[derive(Debug)]
pub struct NativeRunError { pub message: String }
pub struct NativeExit { /* private successful-close marker, constructed only after ReadyToExit */ }
pub fn run(host: PreparedShellHost, options: NativeWindowOptions)
    -> Result<NativeExit, NativeRunError>;
~~~

The conversion matches ui::UserEvent at ui/src/app.rs:143. This single-window enum has no remote raw pointer/app identity messages. Window creation, adapter and callbacks remain main-thread-only.

- [ ] Extend shared window_lifecycle::tests for zero→nonzero, occlusion, duplicate resume, suspension, scale-only change and close phases. Add native_host_tests (library module) for real required-save failures and retained retry; a ClosePhase decision-table test alone cannot prove save ownership.
- [ ] Add native_window_probe with required absolute --scratch directory, --frames positive integer and --scale-report output; it creates an owned explicit fixture host and declares fixture=true in evidence. It uses actual FrameBuilder and NativePresenter. No operator profile is loaded. Run parser/nonwindow tests before implementing the loop.
- [ ] Create EventLoop<NativeUserEvent> once in run on the process main thread, bind the shell WakeScope to a proxy-only sink and bind component scopes via their shared host. Retain prebind wake delivery. user_event checks host/instance lifetime before dirtying; closed/replaced instances cannot wake the new instance. Components do not install their own event loop.
- [ ] resumed creates a hidden window, creates AccessKit adapter while hidden, applies NativeViewHost on the root when supported, then shows it and requests first redraw. Repeated resumed does not recreate components. Initialize GPU on first RedrawRequested; initialization failure sets window title/log plus host unavailable status. No main-thread tokio runtime owns the app loop.
- [ ] Feed window events to AccessKit before shared widget dispatch. Process native callbacks in place. Track raw modifiers, physical cursor/touch/scroll, and use the current FrameViewport.scale for dispatch_event_scaled. Do not synthesize text from physical keys; forward winit keyboard logical key/text and Ime::{Enabled,Preedit,Commit,Disabled} through existing widgets. Test non-ASCII preedit selection/cancellation and exactly one commit.
- [ ] Sync IME after input, focus, layout, resize and scale change, even when the caret's logical rectangle stayed equal. Only the focused active text component enables it. Focus loss disables/cancels native composition and clears pressed/modifier capture; leaving/closing a tile cannot move its preedit into another. Restore focus only to a still-live target.
- [ ] Use shared PlatformConventions::new(Platform::MacOs) for Command/Control behavior and the existing widget propagation. Do not globally remap Command to Control: shell shortcuts and editor commands must share semantic conventions while macOS reserved shortcuts stay host-owned.
- [ ] Use FrameBuilder.build_a11y_tree_update(focus, scale) after valid layout and meaningful semantic changes, through adapter.update_if_active. InitialTreeRequested gets a current tree even before a GPU frame. AccessKit actions route into existing dispatch_a11y_action and bound resource action logic; internal synthetic pointer fallback stays logical/scale-1. Reject stale node/lifetime requests. Real VoiceOver checks remain mandatory.
- [ ] RedrawRequested refreshes geometry and forwards it to the actual WindowLifecycle, pumps allowed service/widget work under scopes, then calls begin_frame. Only an admission permits relayout/build/present; forward every present result to finish_frame before returning. During RecoveryRequired pump only trusted recovery UI and retained persistence owners. No queued redraw-event replay or duplicate lifecycle counters.
- [ ] about_to_wait uses Wait when clean, request_redraw for dirty work and WaitUntil for the earliest shared maintenance/animation/recovery deadline. While animation is active schedule at most one 60 Hz request; use a one-second maintenance tick for clock/theme/status only when those services lack a wake. Native Fs observation already has its own two-second provider cadence. Coalesce wake/redraw requests, never durable operations.
- [ ] On surface Timeout schedule one retry after 50 ms; after three consecutive transient failures park presentation and show its typed status through window title/log and chrome when rendering recovers. Surface/device recreation gets three attempts separated by 250 ms, preserving all components/documents; exhausted recovery parks until a real lifecycle change or explicit Retry Presentation command. A successful presentation resets the streak. No recursive retry from inside present.
- [ ] Resized/ScaleFactorChanged updates geometry and invalidation synchronously. Ignore zero extent for GPU creation; restore triggers dense render. Occluded(true) parks GPU work without closing components. suspended disables IME, detaches native children and drops presenter/surface before window references; preserve component/lifetime unless the shared suspension policy explicitly closes it. resumed remounts native children and creates fresh presentation only. Sleep/wake gets a dense redraw; do not claim winit always emits Suspended on Mac—also handle occlusion/focus/size/device loss.
- [ ] Borderless fullscreen uses existing winit window fullscreen; do not map shell's tile-fullscreen state to Mac Spaces ownership. Verify tile maximize and window fullscreen independently.
- [ ] CloseRequested calls host.request_close exactly once, cancels app IME, marks mounts Closing, captures/closes each AppComponent through its existing owner, removes component node routes/native children and retains every ComponentCloseReport outside widgets. Keep the shell WakeScope, FrameBuilder chrome, window, presenter, AccessKit adapter, profile lock, canonical owners and FilesServiceOwner alive. Repeated CloseRequested returns/publishes the same current outcome, without recapturing destroyed widgets. It never calls event_loop.exit for Pending/RecoveryRequired.
- [ ] request_close checks common View/record reports and the Files owners' actual persistence state. Pending app-local saves or running file operations return Pending; failed required View/record/preference writes or unpersisted operation reports return RecoveryRequired. A durable recoverable file outcome may remain an explicitly recorded recovery item for next launch; it is not relabeled successful execution. No unavailable/unknown report is treated as durability proof. An empty AppLocalStateOwner::shutdown_report and successful required common reports are necessary for ReadyToExit.
- [ ] Reuse the existing shell notification/status surface and action routing to expose a visible accessible “Could not finish saving” status and “Retry saving” action with stable host action ID shell.retry_close. Its callback sets one shared coalesced retry-request flag; after releasing the with_frame borrow, pump_host_controls consumes that flag and calls retry_close. It cannot capture/borrow the owning host recursively during a widget callback. It is shell-owned and enabled while components are closed. Render issue-specific messages from HostCloseStatus; preserve existing file-operation recovery actions rather than invent a first-user/recovery application. Pointer, keyboard and a11y route to the same flag/action. No auto-discard action is added.
- [ ] retry_close retries each retained ComponentCloseReport::retry_view and retained record_retry.try_flush on the exact original owner, clearing only acknowledged successful errors/handles. For each failed AppLocalStateClient save use the retained owner.client().retry(save_id), then poll status; do not replace desired bytes, expected revision or save ID. Poll tracked OperationOwner reports and retain its journal/jobs; never replay copy/move/trash to fix a close. Existing file recovery actions call its explicit resolve with freshly authorized grant/session, while unknown native commit remains unknown. Teardown/capture errors lacking a proven retry remain blocking and visible rather than silently cleared.
- [ ] poll_close reads actual acknowledgements, never reinitializes components or starts new document effects. While Pending schedule a 100 ms host-maintenance wake (or existing owner completion wake); while RecoveryRequired wait for an explicit retry or provider completion. Map outcomes to WindowLifecycle ClosePhase. Only ReadyToExit triggers event_loop.exit; then close shell WakeScope, release native presenter before adapter/window, and finally release service/profile owners. NativeExit represents that successful normal-close path only. Forced OS/process termination and arbitrary process panics remain separate failures, never automatic recovery or safe-close evidence.
- [ ] Test a real component with final record flush and View save failure, plus a Files preference failure: request_close returns RecoveryRequired, weak owner probes still upgrade after app widgets and all other handles are dropped, component wakes/actions are refused, shell retry remains actionable, and window_lifecycle decision.exit remains false. Repair disposable destinations; call the actual retry_close/poll_close until acknowledgement, assert bytes saved to original namespaces with no new factory call and then ReadyToExit. Test repeated close does not clear errors, a pending save cannot exit, and an unresolved file operation keeps its owner. Exercise the retry Button/a11y action on actual FrameBuilder chrome in the library test, not only direct method calls.
- [ ] Native Drop remains a failure/unwind safety net that closes scopes before root destruction. Ordinary CloseRequested must never reach Drop with retained required-save failures. Do not add blocking network joins to callbacks; retain existing bounded service shutdown once close is ready.
- [ ] Run policy tests, build the probe and execute the bounded actual Mac probe under its isolated scratch. Record actual main-thread callback/acquire/present, Metal adapter/backend, dimensions/scale and outcomes. Run Task 5 gates; commit: feat(shell): drive the shared shell from the macos main loop.

## Task 6: Prepare the enrolled in-process host and connect real app definitions

This is the minimal native trusted bootstrap, not first-user enrollment, a process issuer, a separate SDK, or a new collaboration service. It runs after Task 5, consumes its existing PreparedShellHost and the Files Task 6 FilesServiceOwner and adds strict production preparation. The explicit fixture constructor supports adapter development only; production run uses the strict prepare function.

**Consumes:** Common framework; real Notes Tasks 1–3; Files Tasks 1–6 including its concrete service-owner fields; existing identity registry/device fleet/certificates; accepted shared collaboration binding.
**Produces:** prepare_native_host/config parsing and the real target-specific main, constructing Task 5's one PreparedShellHost. It does not define another host container, close implementation or service owner.

~~~rust
// shell::shell::native_host: config is local host policy, never app input.
pub struct NativeHostConfig {
    pub version: u32, // exactly 1
    pub profile_state_root: std::path::PathBuf,
    pub identity_directory: std::path::PathBuf,
    pub collaboration_root: std::path::PathBuf,
    pub file_roots: Vec<ConfiguredFileRoot>,
    pub peer_sync: bool,
}
pub struct ConfiguredFileRoot {
    pub workspace: String,
    pub app_id: String,
    pub path: std::path::PathBuf,
    pub label: String,
    pub rights: Vec<runtime_fs::FsRight>,
}
pub enum NativeHostError {
    InvalidConfig(String), ProfileBusy, MissingIdentity,
    IdentityUnavailable(String), InvalidIdentity(String),
    MissingWorkspace(String), Denied(String), RequiredProvider(&'static str),
    State(String), Component(ui::launch::LaunchError),
}
pub fn prepare_native_host(config: NativeHostConfig)
    -> Result<PreparedShellHost, NativeHostError>;
// Existing network module, strict additive constructor.
impl ShellNetwork {
    pub fn spawn_enrolled(tree: WorkspaceTreeHandle, keypair: iroh::SecretKey,
        identity: crate::network::actor::HostIdentity,
        registry: crate::identity_registry::IdentityRegistryStore,
        fleet: crate::device_fleet::DeviceFleetStore) -> std::io::Result<Self>;
}
~~~

FsRight in the Files producer is not currently specified as Deserialize. Use a private RawNativeHostConfig/RawConfiguredFileRoot with serde Deserialize and deny_unknown_fields; decode rights through a private fixed string enum matching Enumerate, Read, Create, Rename, Copy, Move, Trash and Purge, then map to the exact runtime variants. DirectoryGrant/FileGrant never become deserializable. The public structs above are validated in-memory values. Return typed variants internally; error display may include a path but never key material.

Task 6 fills the existing Task 5 container. The one FilesServiceOwner receives the exact Files Task 6 owners, transfers, tracked operation IDs and reports; the container's canonical RecordOwners and View stores remain the same handles used at component preparation. The fixture constructor returns io::Error and does not refer to these production config/error types. No cycle through Task 6 is allowed in Task 5's build or example.

- [ ] Add config/admission cases to native_host_tests in the library with absolute temporary roots and explicitly enrolled cryptographic fixtures: missing/invalid config, second writer, missing identity, wrong root/device certificate, expired/revoked device, unavailable secret provider, identity/workspace mismatch and grant outside configured authority. Count factory invocations and assert zero on rejected admission; compare all existing document/identity bytes before/after failure.
- [ ] Extend the actual real_files_host_tests library integration through prepare_native_host with an enrolled disposable fixture: actual Notes and Files definitions, exact record open, real filesystem operation, close/reopen, stale wake rejection and retained failures. This is authenticated fixture data exercising production admission, not a replacement factory or permissive authority.
- [ ] Run new failing selections.
- [ ] Parse only version 1 and explicit absolute paths. Default Mac config location is dirs::config_dir()/qos/native-host.json (Application Support on Mac); --native-host-config ABS selects another. Config never contains signing keys or grants serialized as authoritative handles. Missing config returns setup-required, with no identity creation or real-profile fallback. Reuse existing macOS path provider when it exists rather than duplicate its Application Support policy.
- [ ] Take a private nonblocking profile lock before writable workspace/record/view ownership, using the existing Unix flock pattern and close-on-exec File. Verify private directory ownership/mode and reject symlink/nonregular lock destinations. Lifetime is PreparedShellHost. Profile locks do not replace common canonical record locks. Initial support opens a configured existing profile/workspace; it does not silently create a new workspace when requested persisted state is unavailable/corrupt/newer.
- [ ] Load IdentityRegistryStore::at(identity_directory) without migrate-on-read; select each workspace's configured/default identity via existing registry. Load signing material through runtime_identity::load_from_path, honor actual provider protection/error. Derive person principal from the loaded public key, verify registry metadata matches, read IdentityPaths::under(collaboration_root) device secret/cert and per-user workspace certificates, verify signatures/expiry/key equality and DeviceFleet revocations. Private record formats stay unchanged.
- [ ] No load_or_create_keypair, ensure_primary_identity, Linux firstboot/PAM/Secret Service fallback or self-announced label supplies native authority. Missing material is typed MissingIdentity/IdentityUnavailable/InvalidIdentity. Existing explicit broker-file custody can be exercised only as that accurately reported protection backend; never silently convert an unavailable Keychain identity to it. Keychain and multi-device enrollment ceremony remain separate delivery.
- [ ] Set the existing namespace salt from the validated device key before loading/minting any workspace/tile IDs. Load the exact state.json under profile_state_root using existing parser/schema validation; refuse corrupt/newer bytes without bootstrap substitution. Keep fs_acl_path None on Mac: real Files grants come from NativeGrantAuthority, not FUSE export.
- [ ] Build current PermissionContext from the cryptographically selected person/app actor and the workspace's actual capability list. Reread current workspace/identity/revocation authority before each admission and update each live Rc<RefCell<PermissionContext>> before callbacks/actions; service effects also check current authority at their own execution/chunk/commit doors. Expiry is refreshed using actual current time, not a startup-frozen permission snapshot. Never use permissive(), local_user_id() fallback or owner inferred solely from configuration text.
- [ ] Reuse Files Task 6's one in-process service owner. Open configured file directories as owned descriptors only after host membership/app/root policy admits them; bind native grants to exact owner and mount UUID, attach the exact HostContext lifetime, and pass FilesContext::new(...).with_record_services(...). Do not convert PickerScope or restored path strings into grants. Own OperationOwner/AppLocalStateOwner/FileTransfers beyond widget close and preserve their report/retry semantics. Configured roots represent explicit trusted host choices; chooser acquisition and external URL security scope are not thereby qualified.
- [ ] Link one ApplicationCatalog with notes::definition() and file_explorer::application_definition(), taken from actual app crates. Resolve package ID through it; preserve remote package catalogue and Linux process launching separately. Unknown/unavailable fleet entries render the existing retryable missing-provider tile with preserved app/record/instance binding. Do not register shell::notes_app under dev.qos.notes.
- [ ] Construct HostContext::prepare with exact RecordOwners store, Rc<InstanceViewStore>, authenticated device/instance/author/current permissions/context and LaunchTarget. Invoke definition.instantiate exactly once. Bind/mount through the same TileHost::from_component path used by Files Task 6; later sync/repaint never re-instantiates. New record creation requires an explicit user operation; failure after Ready retains LaunchError::ComponentSetup and retries Existing.
- [ ] Persist real app ID, record checkout and durable instance binding through existing WorkspaceTree persistence/placement. Two tiles opening the same record share the canonical owner and keep separate device-local Views. Remove callbacks/node bindings before close/replacement and retain ComponentCloseReport independently of removed widgets, plus Files operation/preference recovery reports. Closing one tile cannot discard another's canonical owner.
- [ ] Extend ShellNetwork through spawn_enrolled using existing Actor/protocol/crypto; reject invalid supplied identity before endpoint binding and never use host_identity_for_key's Option fallback. Pass explicit registry/fleet to certificate refresh instead of discovering env on heartbeats. Preserve legacy spawn behavior for Linux callers. Reuse existing shell collaboration broker/action transport only where required by actual shared record/presence integration; do not open a new native-only socket/protocol to authenticate already trusted linked code.
- [ ] Attach collaboration once per canonical record owner and instance-specific presence/follow routes per component. If the accepted record transport is still unavailable, expose RequiredProvider("record collaboration") in advertised capability/status; do not announce synced/live success. Local offline editing remains valid for an authenticated already-admitted record where policy allows it. End-to-end collaboration gate remains failed/not-run until actual transport is exercised.
- [ ] In main move original Linux startup into linux_main.rs behind cfg(linux), keeping flags/ordering/errors verbatim. Mac main validates only --native-host-config ABS and --fullscreen (plus --help/version if existing shared conventions provide them), then prepares and runs the native host. Reject Linux --mode/--backend/firstboot/portal flags with exit 2. Existing Linux --backend headless keeps the original path and never initializes AppKit.
- [ ] Before enabling native --all-targets, audit each shell integration module and example currently reached by that command. Record Linux-only protocol/device/session dependencies and cfg only those true Linux integrations at their individual module/entry boundaries, keeping them fully compiled/tested on Fedora. Every portable integration continues on Mac, including the new library tests; no whole tests/it.rs Linux gate and no ignored/empty portable test modules. Platform-specific examples may expose an explicit unsupported-target error on Mac, never successful stub output. Port any ordinary shared dependency revealed by this audit rather than dropping its coverage. The actual Mac production main and all affected examples/portable integration targets must compile before Task 6 acceptance.
- [ ] Native setup/admission failure prints typed actionable state and exits nonzero before app factory/window creation; adapter probe remains available for window development. A retryable component/provider failure after host admission appears in existing shell chrome/error tile, retaining its data. This avoids inventing a new first-user screen inside this bounded runner task.
- [ ] Run Task 6 gates and commit: feat(shell): prepare an enrolled native host with real app components.

## Task 7: Qualify the real native slice and preserve the same-SHA Fedora path

**Consumes:** Completed Tasks 1–6 and real Notes/Files producer commits.
**Produces:** Evidence that distinguishes adapter, real apps, host providers, collaboration and standalone support.

- [ ] Add/update exact real shell headless scenarios using the same linked catalogue/preparation, preserving existing Linux action schema, semantic inspection/output files and failure exit meanings. A headless library host receives explicit profile/identity/grants; it never creates EventLoop or reads interactive defaults. Keep rendered offscreen and semantic-only requirements separate.
- [ ] Execute physical Mac window checks with enrolled disposable profile, real Notes and real Files: launch inside one window; focus/create/edit; text selection/copy/paste; Command shortcuts; two Notes views via exact Files record; palette/a11y document action; independent undo; close/reopen and restore after restart.
- [ ] Verify real IME with an installed non-Latin input method: preedit/candidate positioning, replacement range, commit once, Escape cancellation, click between tiles during composition, resize and display move during composition. Synthetic event tests alone are insufficient.
- [ ] Verify VoiceOver roles/names/text editing/actions/focus order and focus return after modal close at 1×/2×. Use the actual Mac window adapter, not only a semantic JSON dump.
- [ ] Resize continuously, move between Retina/external displays, enter/leave window fullscreen, maximize a tile, minimize/restore, occlude/unocclude, lock/unlock host, sleep/wake and close during background work. Record exact physical/logical dimensions, scale, target generations and no duplicate factories/stale delivery. Device loss recovery uses a real supported diagnostic trigger when available plus actual-path fault tests; report unavailable real trigger instead of pretending it occurred.
- [ ] Use explicit disposable configured directories for Files enumerate/create/rename/copy/move/trash and denial/revocation/partial outcome. Native Mac Trash, original filename, real two-volume moves and chooser/security-scoped URL acquisition have distinct provider evidence; missing prerequisites remain not-run/blockers. Never enumerate or empty unrelated Trash.
- [ ] Run Mac↔Fedora actual Notes collaboration: create from Mac, discover/open from Files on Fedora, simultaneous edits, follow/unfollow, role revocation, offline edit/reconnect and restart recovery. Same canonical record identity and signed authenticated transport must be observed. A local-only native pass cannot mark this walkthrough complete.
- [ ] Run same exact source SHA/lockfile on Fedora for library tests, real-app semantic/rendered headless flows, Linux nested compositor and relevant physical DRM checks if touched. No target exclusions or filtered fake app substitutes. Run required workspace/nightly matrix per main spec when the environment window is available.
- [ ] Record every gate as pass/fail/not-run/not-applicable, include host/toolchain/SHA/command/exit/discovered counts and evidence artifacts. Missing adapter/display/provider is not a pass. Update Notes/Files capability-map rows with bounded claims, not a blanket “macOS supported.”
- [ ] Commit evidence/doc changes after successful relevant gates: test(shell): qualify native mac window and shared real app paths. After a normal git push await automatic final CI notification, investigate with fgj actions, and keep relevant CI green before merging.

## Required execution gates

No commands below were executed during planning. The sole implementation author runs native focused gates before each source commit. The controller reviews that commit and mirrors the exact SHA to Fedora; no source copies or mismatched worktrees count.

Mac shell commands use this prefix, with the existing isolated helper and explicit target:

~~~sh
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py
~~~

For ui-only commands omit PKG_CONFIG_PATH if not needed by its dependency graph. Fedora uses the controller-established /home/parallels/qos-target and its actual ffmpeg environment, not Homebrew paths. Preserve HOME/toolchain; the helper alone does not isolate direct home reads, hence explicit factories/roots.

Source-backed early gate limit: shell/src/main.rs still imports Linux-gated hosted/native orchestration; shell/Cargo.toml has an explicit it integration target and autotests=false; tests/it.rs includes real Wayland/DRM/session integration modules. Cargo integration test builds can build package binaries. Therefore no early native shell --test it or --all-targets claim is made. The commands below deliberately exercise real library code until Task 6 delivers the actual main and individual target classification. This is a sequencing limit, not a permanent portability exclusion.

| Task | Focused nextest payload on both hosts unless labeled Mac | Mac check/lint payloads | Fedora check/lint payloads |
| --- | --- | --- | --- |
| 1 | cargo nextest run --locked --profile ci -p shell --lib -E 'test(native_viewport_tests)'; existing shared frame retained/dense library selections | cargo check --locked -p shell --lib; cargo clippy --locked -p shell --lib -- -D warnings | cargo check --locked -p shell --all-targets; cargo clippy --locked -p shell --all-targets -- -D warnings |
| 2 | cargo nextest run --locked --profile ci -p shell --lib -E 'test(native_component_dispatch_tests)'; cargo nextest run --locked --profile ci -p ui --test it -E 'test(bound_component_dispatch)' plus existing UI IME/resource/undo selections | cargo check --locked -p shell --lib; cargo clippy --locked -p shell --lib -- -D warnings; cargo check --locked -p ui --all-targets; cargo clippy --locked -p ui --all-targets -- -D warnings | cargo check --locked -p ui -p shell --all-targets; cargo clippy --locked -p ui -p shell --all-targets -- -D warnings |
| 3 | cargo nextest run --locked --profile ci -p ui --lib -E 'test(gpu::) or test(render_thread::)' | cargo check --locked -p ui --all-targets; cargo clippy --locked -p ui --all-targets -- -D warnings | Same ui all-target payloads |
| 4 | cargo nextest run --locked --profile ci -p shell --lib -E 'test(window_lifecycle::tests)'; GPU producer's actual library selections; Mac additionally cargo nextest run --locked --profile ci -p shell --lib -E 'test(native_window::presentation::tests)' | cargo check --locked -p shell --lib; cargo clippy --locked -p shell --lib -- -D warnings | cargo check --locked -p shell --all-targets; cargo clippy --locked -p shell --all-targets -- -D warnings |
| 5 | cargo nextest run --locked --profile ci -p shell --lib -E 'test(window_lifecycle::tests) or test(native_host_tests)'; actual Mac probe separately | cargo check --locked -p shell --lib --example native_window_probe; cargo clippy --locked -p shell --lib --example native_window_probe -- -D warnings; cargo build --locked -p shell --example native_window_probe | cargo check --locked -p shell --all-targets; cargo clippy --locked -p shell --all-targets -- -D warnings |
| 6 | cargo nextest run --locked --profile ci -p shell --lib -E 'test(native_host_tests) or test(native_component_dispatch_tests) or test(real_files_host_tests)'; after real main/target audit, run affected actual integration targets | cargo check --locked -p shell -p notes -p file-explorer --all-targets; cargo clippy --locked -p shell -p notes -p file-explorer --all-targets -- -D warnings | Identical all-target payloads |
| 7 | Full affected shared app/runtime/shell tests plus physical matrix and actual cross-host walkthrough | Affected all-target/examples and main-spec full workspace/nightly requirements | Same applicable workspace/nightly and Linux compositor/headless gates |

Run cargo fmt --all --check on both hosts for every task. New shared shell tests, including Files Task 6 real_files_host_tests, are library modules registered above. Existing integration targets remain in tests/it.rs; native Task 6 individually audits their platform prerequisites before its required all-target gates. Portable policy/geometry/host tests remain outside Mac module cfg. The probe example is a Mac-only window role; its non-Mac entry reports Unsupported and exits nonzero if invoked, while Fedora still compiles it through all-target checks. No successful no-op main replaces the actual shell binary. Honor current nextest GPU scheduling and list affected exact test names before filtering; nonzero discovered/executed counts are mandatory. Keep Linux redraw_bench readback/XRGB completion boundary and mean ≤25 ms / p99 ≤50 ms gates distinct from native GPU-only timings.

For the final probe invocation use the built artifact emitted by the isolated Cargo build, an explicitly created absolute temporary scratch directory and the exact accepted flags; record binary SHA. Do not run the production native config against the operator's profile as an accidental smoke test.

## Concrete remaining dependencies and limitations

1. The accepted RetainedGpuTarget and common framework are plans, not code at this inspected SHA. Their implementation precedes relevant consumer tasks; native target calls cannot be implemented against an imaginary API.
2. Common BoundNodeMap currently has proposed internal use in App/Driver. Task 2 explicitly makes its same implementation usable by FrameBuilder. This is the principal producer signature preflight; leaving frame raw callbacks unbound would invalidate real app hosting.
3. Files Task 6 produces FilesServiceOwner in hosted_inputs.rs and runs actual shell integration as library tests. Native Task 5 imports that owner into PreparedShellHost; native Task 6 adds strict production preparation. This sequence has no native-main prerequisite for Files Task 6 and no duplicate provider, canonical registry or file-effect owner.
4. The native window requires no standalone issuer/preparer. It does require Task 6's strict existing identity/config/profile admission. Initial native configured-profile support intentionally returns MissingIdentity/setup-required for a new user; first-user enrollment is a separate accepted product requirement.
5. Keychain, native chooser/security-scoped URL lifetime, native trash/cross-volume guarantees and actual collaboration remain required for the full Notes/Files milestone. Typed unavailability preserves honest partial support; it does not complete the milestone.
   Production poll_handshake/take_completed_identity wiring is not established by the existing full-DKG tests; the configured PairingBrokerServer path currently serves invites. First-user/add-device UI and enrollment completion remain unqualified. A legitimately prepared existing identity permits this runner without asserting those flows work.
6. No software timeout can preempt a stalled backend's configure/acquire/present or scene encoding. Explicit target waits/retries are bounded and callback-stall telemetry is required. Validate this main-thread first implementation on actual Metal before considering worker presentation.
7. A retryable file/record save report must remain owned on failure; process termination still cannot promise unsaved volatile edits survived. Existing durable writer/recovery semantics, not graceful exit alone, establish crash recovery.
8. Native external applications and independently installed packages are not qOS tile hosting. Preserve Linux's package route; no new Mac process bridge is included.

## Self-review

The plan covers spec §7's main-thread loop, retained GPU presentation, coordinate boundaries, callback-synchronous lifecycle, IME/a11y and headless preservation. It consumes common factories/lifetimes and real Notes/Files rather than shell substitutes. Identity/profile bootstrap uses existing registry/device formats and explicit in-process admission, with no dependency on standalone credentials. Same-device target ownership, all-surface blit, failure serials and bounded native retries are specified without altering Linux waits. Provider gaps, production first-user setup, physical qualification and cross-host collaboration remain explicit; this candidate does not claim their completion.

## Task/interface self-review after controller preflight

| Finding / contract | Producer → consumer | Revision and executable gate | Remaining qualification |
| --- | --- | --- | --- |
| 1. Early native target cycle | Tasks 1/2 shared test modules; Task 4 WindowLifecycle library; Task 5 explicit example → Mac library/example commands | New tests register under shell/src/shell/mod.rs or colocated library tests. Mac Tasks 1–5 never invoke shell --test it/--all-targets. Fedora retains full target checks. Task 6 ports real main and individually audits true Linux integrations before mandatory Mac all-target gates. | No early all-target success claimed; actual Mac main/all-target pass required at Task 6. |
| 2. Failed ordinary close lost recovery | PreparedShellHost request_close/retry_close/poll_close → WindowLifecycle ClosePhase → native callback | Pending/RecoveryRequired retain window, shell chrome/scope, profile lock, ComponentCloseReports, canonical/View stores and one FilesServiceOwner. Existing shell.retry_close action queues a host control; successful acknowledgement alone permits ReadyToExit. Library tests keep weak owner probes alive after component removal and execute real retry/action routing. | Native close-with-failed-save walkthrough remains required; forced process termination is not normal safe close. |
| 3. Host ownership cycle | Files Task 6 FilesServiceOwner/library integration → native Task 5 PreparedShellHost/fixture(io::Error) → native Task 6 prepare_native_host | Files produces the concrete owner in hosted_inputs.rs and tests it through the shell library, without native main. Native Task 5 imports it and owns close/retry; Task 6 adds production admission. | Controller reconciled both producer and consumer plans; no duplicate owner, extra provider, issuer or dependency cycle. |
| 4. Bound dispatcher export drift | Common-framework Task 3 ui::bound_dispatch public new/rebuild/clear/invoke_bound_node and typed BoundDispatchError → FrameBuilder Task 2 | Exact crate-relative producer declaration is in Task 2 and adopted verbatim into common-framework Task 3. One ui implementation handles raw-pointer lifetime, closed refusal and same-record reentry. Common guarded semantic/resource traversal retains synthetic originating binding. Shell library tests cover raw descendants plus synthetic origin. | Producer-plan adoption is recorded; its implementation must precede Task 2. |
| 5. Fedora policy tests did not own real callbacks | Task 4 shell::shell::window_lifecycle::WindowLifecycle → Mac ApplicationHandler Task 5 | Owner is unconditionally compiled and tested on both hosts. Mac callbacks delegate admission, serials, retry/occlusion and close phase to that exact object, without parallel counters. Metal/blitter tests are separately Mac-gated and actually executed on Mac. | Library policy tests do not replace native acquire/present/IME/VoiceOver evidence. |
| Surface/Retina contract | GPU RetainedGpuTarget + FrameViewport + WindowSurface → NativePresenter | Accepted same-device target, physical outward damage, complete swapchain-image blit and single caller deadline are unchanged. Geometry/library and real Metal oracle gates remain. | No Linux phase timeout alteration; real device/display checks remain required. |
| Real app/authentication scope | Existing enrolled identity + common factory/Files owners → native Task 6 | No new first-user/DKG flow, process issuer or standalone predecessor. Real Notes/Files definitions and typed unavailable record collaboration remain explicit. | First-user, Keychain, native acquisition and actual authenticated Mac↔Fedora record transport remain separate unqualified gates. |

Self-review performed through read-only source/plan checks and a scan for stale close-bool, duplicate container ownership and early integration commands. No builds, tests, GUI, VM, source/index/HEAD changes or commits were performed for this revision.
