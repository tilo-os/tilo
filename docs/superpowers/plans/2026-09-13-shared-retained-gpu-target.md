# Shared Retained GPU Target Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. One source author at a time owns each complete candidate, performs native tests and self-review, and commits it. The controller then reviews that commit and validates the exact SHA on Fedora. Do not start while the direct shell core cutover has its source author active.

**Goal:** Expose the actual shell retained renderer as a same-device GPU target with bounded, truthful completion, preserve Linux XRGB output, and measure a separately identified 1,000-action 4K shell workload without framebuffer readback.

**Architecture:** Extract target ownership and normal render/recovery from the real shared paint module. Linux's bridge keeps its public API and framebuffer-output transaction; a GPU-only consumer receives a settled texture view. Extend the existing Vello bump tracker to distinguish pending, completion and failure instead of creating a parallel renderer or speculative scheduler.

**Tech Stack:** Existing Rust, wgpu 29/Vello fork, `ui::gpu::RenderContext`, production `FrameBuilder`, nextest.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md` §§9–11; `docs/superpowers/plans/2026-09-13-direct-shell-core-cutover.md`.

Plan status: revised after controller preflight, 2026-09-13; execution follows the reviewed direct core candidate. No production code, build, tests, git, VM or GUI operations were performed for this revision. Read-only Cargo package selection and nextest help were inspected; they are not runtime validation. Public names/signatures introduced below are proposed implementation contracts, not claims about current APIs. All shell paths below are the **post-core-cutover** owners; the inspected originals were `shell/src/compositor/frame.rs` and `shell/src/compositor/backend/drm/{paint,xrgb_history}.rs`. Reconcile against the reviewed core commit before editing, without restoring old ownership.

## Global constraints

- “Reuse WorkspaceTree, shell chrome, actions and FrameBuilder as the scene producer.”
- “Benchmarks and tests must not load the user's real state or secrets.”
- “Keep Linux defaults and HDR behavior.” Preserve public Linux re-exports, `redraw_bench`'s completion + readback + XRGB boundary and mean ≤25 ms / p99 ≤50 ms gates.
- One normal production Vello render/recovery implementation. Total Vello submission-attempt budget remains **4**, including an attempted sparse pass and its dense recovery. No shader rewrite, dependency upgrade, new crate or OS-selection feature.
- GPU-only operation allocates no framebuffer staging buffer and executes no framebuffer copy/map/swizzle. Tiny bump-counter readback remains required recovery metadata and must be reported separately from framebuffer readback.
- No native window, surface configuration/blitter implementation, app factory migration, Keychain provider or full app-host qualification in this slice. Only define the API the later native runner consumes.
- Pixel oracles run outside timing, require a real adapter and retain existing tolerance/assertions. Missing adapter, timeout, soft skip or incomplete sample set is failure, not performance evidence.
- Controlled font injection and `docs/superpowers/plans/2026-09-13-{typography-renderer,typography-fixture-qualification}.md` remain prerequisites for cross-host pixel qualification. Existing explicit catalog loading does not mean FrameBuilder's default text renderer is controlled.
- Runtime commands go through `.superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py`; preserve nextest process isolation, GPU scheduling and the isolated visual-golden tail. HOME preservation is not proof of isolation.
- Mac author uses `/Users/xaver/dev/qos/target/macos-shell` and environment-specific `PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig`; never put that path in portable source. Controller alone mirrors committed SHAs to `/home/parallels/qos-macos-shell`, using `/home/parallels/qos-target` on Fedora.

## File and API map

| File | Responsibility |
| --- | --- |
| `vendor/vello/src/bump_budget.rs` | Keep actual counter observation/growth; add explicit checked collection and sticky failures, with tests against real tracker/download ownership. |
| `vendor/vello/src/lib.rs` | Nonblocking checked settlement facade, overflow/error retention across opportunistic collection, compatibility of existing APIs. |
| `ui/src/gpu.rs` | One device-loss observation per actual context-owned device, shared with its target consumers; do not let individual targets replace the device's callback. |
| `shell/src/shell/render/paint.rs` | Actual target-side extraction, bounded completion, optional framebuffer output, existing bridge facade and diagnostics. A private sibling `target.rs` is permitted only if it moves this implementation without duplicating it. |
| `shell/src/shell/render/xrgb_history.rs` | Existing history logic and regression tests; change only if necessary to carry the preserved bridge identity/epoch boundary. |
| `shell/src/shell/frame_retained_tests.rs` | Existing actual FrameBuilder retained/full oracles plus target tests. Preserve assertions and test discovery after relocation. |
| `shell/examples/shared_gpu_bench.rs` | New common benchmark and summary/output tests. Keep this separate from the large existing Linux diagnostic example. |
| `shell/examples/support/shell_gpu_fixture.rs` | Explicit versioned, service-free shell-chrome fixture, input sequence and profile validation shared only where justified. |
| `shell/Cargo.toml`, `.config/nextest.toml` | Register the portable example if required and serialize actual GPU tests using current naming after the move. |

The native runner will consume this proposed interface, using the `DeviceHandle` selected against its surface. Device and queue come together from that one handle; do not accept unrelated device/queue parameters or secretly open another context.

```rust
// shell::shell::render::paint; proposed additions
pub struct RetainedGpuTarget { /* owns private device/queue clones, renderer, target */ }

pub struct GpuFrameStats {
    pub revision: u64,
    pub damage_admitted: bool,
    pub render_attempts: usize,
    pub sparse_attempts: usize,
    pub full_attempts: usize,
    pub completion_markers: usize,
    pub framebuffer_copy_bytes: u64,
    pub framebuffer_staging_bytes: u64,
}

impl RetainedGpuTarget {
    pub fn new(gpu: &ui::gpu::DeviceHandle, width: u32, height: u32)
        -> Result<Self, BridgeError>;
    pub fn set_base_color(&mut self, color: vello::peniko::Color);
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), BridgeError>;
    pub fn render_until(&mut self, scene: &vello::Scene, damage: SceneDamage<'_>,
        deadline: std::time::Instant) -> Result<GpuFrameStats, BridgeError>;
    // None before completion, after invalidation or on failed/poisoned output.
    // Read-only sampling contract: consumer may blit, not mutate this target.
    pub fn completed_view(&self) -> Option<&vello::wgpu::TextureView>;
    pub fn size(&self) -> (u32, u32);
}

// Vello; proposed checked API in addition to the compatibility method
pub enum BumpBudgetPoll { Pending, Complete { overflowed: bool } }
pub enum BumpBudgetError { MapFailed, MissingDownload, InvalidDownload, Poll(String) }
impl Renderer {
    pub fn poll_bump_budget(&mut self, device: &wgpu::Device)
        -> Result<BumpBudgetPoll, BumpBudgetError>;
}
```

Keep error variants in the existing `BridgeError`: add `DeadlineExceeded`, `BumpReadback(String)`, `DevicePoll(String)`, `DeviceLost(String)` and `RecreateRequired`, rather than reporting them as successful empty frames. Keep existing variants and signatures usable. `completed_view` is only target completion, never a claim about display presentation. Public diagnostic wrappers for adapter information, sparse stats and Vello memory/timestamps forward existing machinery; define their exact set when moving the existing bridge accessors, not by guessing new hardware telemetry.

## Task 1: Make existing Vello settlement truthful and nonblocking

**Files:** `vendor/vello/src/{bump_budget,lib}.rs`, their existing test modules. Tests select the patched Vello package from root `Cargo.toml`/`Cargo.lock`; do not change either manifest or lockfile for test plumbing.

**Consumes:** `BudgetTracker::collect`, `begin_readback`, `has_pending_readback`, actual download buffers, `Renderer::unsettled_bump_overflow`.

**Produces:** the checked `poll_bump_budget` contract above; existing `settle_bump_budget` remains source-compatible for callers outside this slice.

- [ ] Read the completed core candidate and record its SHA. Inspect current Vello call sites before editing: `collect` presently returns `(changed, overflowed)`, restores not-ready pending state, and silently treats failed maps/missing downloads as no overflow. `begin_readback` also silently returns for a missing download. Record these three distinct failure sites in the task report.
- [ ] Add checked collection using the actual tracker and engine. `Pending` retains the download and accumulated overflow; completed valid counters preserve existing observation, buffer recycling and pool-clear rules. Map failure, absent download, invalid mapped length and failed device poll are explicit errors. A failure discovered by ordinary opportunistic `collect` must remain latched until strict settlement sees it; later successful polling must not erase it. Keep legacy callers compatible by delegating their tuple view to the same collection implementation, without consuming the error latch. No copy of the growth algorithm.

```rust
// Required facade behavior; concrete tracker storage remains private.
match renderer.poll_bump_budget(&device)? {
    BumpBudgetPoll::Pending => { /* no completion / no overflow-latch reset */ }
    BumpBudgetPoll::Complete { overflowed: false } => { /* counters settled */ }
    BumpBudgetPoll::Complete { overflowed: true } => { /* retry required */ }
}
// No wait_indefinitely call is reachable through this new API.
// A fresh idle renderer with no render/readback started has no outstanding
// counter work and may report Complete. Otherwise Complete without counters is
// permitted only when measurement is deliberately disabled. An expected but
// absent download is an error, never idle. Neither case proves GPU/frame
// completion; the target owner independently fences actual submissions.
```

- [ ] Add tests named `checked_bump_pending_retains_download_and_overflow`, `checked_bump_map_failure_is_not_completion`, `checked_bump_missing_download_is_error`, and `checked_bump_collection_preserves_growth_and_recycles_once`. They operate on `BudgetTracker`, the real download allocation and `Renderer`; use narrow `cfg(test)` access to the pending callback/result to hold readiness or report a failed map. Do not test a replacement enum/state-machine model. Assert no duplicate recycle and no poisoned buffer returns to the pool. Exercise the case where an ordinary render's collection observes failure before the checked caller runs.
- [ ] Add a real GPU render test comparing strict settled counter/growth results with a completed existing dense render. Add tracker-only `checked_bump_disabled_measurement_reports_no_counter_work`: deliberately disable measurement and verify no counter download is requested and checked settlement reports no counter work/overflow. This asserts nothing about GPU completion. The independent fence assertion belongs to Task 2, after the target exists. Show the new failure tests fail for current collapsed behavior, then pass with the implementation. GPU test setup must fail explicitly if no adapter is available.
- [ ] Run vendor tests using root `--manifest-path Cargo.toml -p vello` with root `--config-file .config/nextest.toml --profile ci --locked` and one test process at a time (commands below), format the changed files, self-review all `collect`/`begin_readback` paths and native-check its shell consumer. Commit this complete candidate. The controller reviews it and validates the same vendor tests on Fedora at that SHA before the next author edits its consumer. Do not stage this SDD plan.

## Task 2: Extract the actual target and retain Linux's output transaction

**Files:** `shell/src/shell/render/paint.rs` (optional private sibling `target.rs`), `shell/src/shell/frame_retained_tests.rs`, `shell/src/shell/render/xrgb_history.rs` and its tests, **`ui/src/gpu.rs`** for context-owned loss observation; `.config/nextest.toml` for actual GPU test names.

**Consumes:** Task 1 checked settlement, existing `SceneDamage`/`FramePaint`, `ui::gpu::DeviceHandle`, the whole current bridge's dense/sparse submission and recovery implementation.

**Produces:** `RetainedGpuTarget` above and an API-compatible `VelloDrmBridge` delegating to it.

- [ ] Move renderer/target/revision ownership into the target. Reuse the existing `Rgba8Unorm`, `AaConfig::Area` and renderer options; add `TEXTURE_BINDING` to the target's current `STORAGE_BINDING | COPY_SRC`. Reject zero/oversized extents before allocation. `new` clones the supplied handle's device and queue together; the bridge retains context/adapter ownership and passes its selected handle rather than opening an additional GPU.
- [ ] Install device-loss observation once in `ui::gpu::RenderContext` when its `DeviceHandle` is created. Proposed narrow API: `DeviceHandle::loss_monitor() -> DeviceLossMonitor`, a cloneable read-only observer, and `DeviceLossMonitor::reason() -> Option<String>`. Its callback records the actual wgpu loss reason; target creation stores that observer and checks it before submission and publication. Reconcile any existing callback ownership before adding this; targets must never overwrite one another's device callback. wgpu 29's `PollError` only reports timeout/wrong-submission-index, so do not claim polling alone detects device loss. A loss invalidates target completion and returns `DeviceLost`, requiring a new device rather than a same-device renderer retry.
- [ ] Factor one internal output choice into the actual normal render attempts: GPU-only appends nothing; XRGB appends the existing full/sparse texture-to-staging copies in the same Vello post-render callback. Preserve the existing single-submit Linux copy ordering and swizzle/history transaction. Do not implement Linux by calling public `render_until` and then performing another full render or unconditional full copy. Leave explicit paired replay/geometry diagnostics available through their existing bridge facade.
- [ ] Track target validity and staging validity independently, each with content revision/geometry generation. Sparse target admission requires the preceding successfully completed target revision, existing normalized damage and sparse support; it does not require CPU staging. Sparse XRGB readback additionally requires matching complete staging. Missing/stale staging forces a **full staging refresh from the current completed target**, or the existing conservative dense boundary, never partial repair over stale bytes. The bridge may retain its stricter invalidate-both-on-output-failure rule. Keep XRGB history bridge generation/epoch, pitch/target count and scanout epoch behavior unchanged.
- [ ] Add one private completion policy to the shared render/recovery implementation. Public GPU-only `render_until` selects `Absolute(deadline)`. The existing Linux paint facade selects fixed `LinuxPerPhase` internally; it does not delegate through GPU-only `render_until`, take a new public timeout parameter, or inherit a one-second whole-frame limit. Keep output selection and wait policy private so this does not grow into a configurable scheduling framework.

```rust
// Proposed private policy; both routes still use the same actual render attempts.
#[derive(Clone, Copy)]
enum CompletionPolicy {
    Absolute(Instant), // supplied only by the GPU-only caller
    LinuxPerPhase,    // existing bridge facade's fixed policy
}
const LINUX_FENCE_WAIT: Duration = Duration::from_secs(1);
const LINUX_MAP_CALLBACK_WAIT: Duration = Duration::from_secs(1);
const BUMP_SETTLEMENT_WAIT: Duration = Duration::from_secs(1);
// RESILIENT_RENDER_ATTEMPTS remains 4, shared across sparse + dense recovery.
```

  Source-based ruling: normal dense and sparse helpers both call `map_readback`, which gives `device.poll(Wait)` **one second**, then `rx.recv_timeout` a **fresh one second** (`shell/src/shell/render/paint.rs`, inspected lines 2130–2170). Each overflow retry repeats those phases, up to four submissions. `Renderer::settle_bump_budget` then performs up to four **indefinite** polls (inspected vendor lines 3148–3165); its comment does not make them finite. Preserve each existing Linux one-second phase allowance and its visible failure, and replace only this unbounded normal-frame settlement with a new **one-second counter-settlement phase per submitted attempt**, beginning after framebuffer map completion. This new cap adopts the established phase duration; it is not an increase to the existing fence or callback timeout. No resetting the settlement deadline on `Pending`, no four fresh settlement deadlines and no additional submission when any phase fails. Only a successfully completed overflow result permits the next recovery submission.

  The derived maximum is **4 × (1s fence + 1s framebuffer callback + 1s counter settlement) = 12s of explicit waits per logical Linux frame/recovery sequence**. This is an upper bound on requested bounded waits, **not a twelve-second wall-clock frame guarantee**: scene encoding, queue submission, driver/callback execution, CPU copy/swizzle and OS descheduling are not preempted. No absolute twelve-second guard is imposed on Linux. Existing one-second fence-timeout failures, including the pending Fedora regression, remain failures; this arithmetic does not diagnose or fix them. Keep any already-bounded diagnostic-specific policy (`map_replay_buffers` already uses an absolute one-second deadline) unchanged. The twelve-second accounting applies to normal dense/sparse framebuffer output, not multi-output diagnostic methods.

- [ ] For GPU-only output, current Vello render methods do not return their queue submission index: append one empty ordered queue submission after each actual attempt and count this completion marker separately from Vello attempts. Wait for that `SubmissionIndex` using only the caller deadline's remaining time, then require checked bump settlement using the **same** deadline across all recovery attempts. Check expiration before actual Vello submission, after encoding, before the marker, after every poll and before publication. An already-expired call invalidates any publishable view and submits nothing. The marker proves submission completion even with measurement disabled. For XRGB output, keep the existing appended-copy/map completion fence and strict settlement under `LinuxPerPhase`; do not add a marker submission or move the copy into a later submit.

```rust
// GPU-only completion branch, after checking that the caller deadline is live.
let marker = queue.submit(std::iter::empty::<wgpu::CommandBuffer>());
let remaining = deadline.saturating_duration_since(Instant::now());
if remaining.is_zero() { return Err(BridgeError::DeadlineExceeded); }
device.poll(wgpu::PollType::Wait {
    submission_index: Some(marker),
    timeout: Some(remaining),
}).map_err(|error| match error {
    wgpu::PollError::Timeout => BridgeError::DeadlineExceeded,
    other => BridgeError::DevicePoll(format!("{other:?}")),
})?;
// poll_bump_budget uses Poll only. Loop until Complete or the active deadline.
// For Linux that deadline starts once, on entry to counter settlement, at now+1s.
// For GPU-only it remains the original caller deadline. Check the loss observer
// and deadline before publishing; yield with a delay capped by remaining time.
```

  A submission fence and bump settlement are two required facts; neither alone proves usable pixels. Preserve sticky errors and overflow through all opportunistic and checked collection. A pending counter never permits output publication. Use a bounded sleep/yield between pending checks, never a busy loop or `wait_indefinitely`. Phase timeouts identify the failed phase (existing `Map` fence/callback errors remain recognizable; new counter timeout includes its phase), invalidate completion and stop that call. Record wait policy/phase in diagnostics so a Linux phase timeout cannot be mislabeled as a GPU-only absolute deadline.
- [ ] Preserve total attempt accounting: at most one admitted sparse attempt; overflow/unavailable sparse work selects dense recovery; all actual Vello submissions share the cap of four. Unsupported sparse selection without a submission does not consume a GPU attempt. Deadline, poll, counter or exhausted-recovery failure invalidates target completion and XRGB publication. A renderer with unresolved/error counter state is not reused: GPU callers reconstruct it; the legacy bridge rebuilds poisoned renderer/target state before its next retry and advances its history generation/boundary. Do not recycle a timed-out download into later work while its callback can still arrive.
- [ ] Preserve resize semantics, including the current bridge's rejected/no-op resize invalidation, changed base-color invalidation and unversioned full paints not seeding a later versioned sparse relation. Do not reset clear color every sample when unchanged. Sparse normalization keeps the current rectangle/count/area rules; no broadening thresholds or looser pixel checks.
- [ ] Add actual GPU tests `gpu_target_sparse_without_staging_matches_dense`, `gpu_target_revision_gap_and_resize_force_dense`, `gpu_target_failure_hides_view_until_reconstruction`, `gpu_target_xrgb_staging_gap_gets_complete_refresh`, and `gpu_target_overflow_uses_four_attempt_budget`. Seed a real target, change a bounded scene region, read the target only after `render_until` returns, and compare with independently dense output using existing helpers. For no-staging reuse assert both observed target allocation reuse and `framebuffer_staging_bytes == 0`; verify pixels outside actual damage remain byte-exact. No normal-frame readback may hide inside a test-only accounting abstraction.

```rust
let outcome = target.render_until(&changed_scene, changed_damage, deadline)?;
assert!(outcome.damage_admitted);
assert_eq!(outcome.framebuffer_copy_bytes, 0);
assert_eq!(outcome.framebuffer_staging_bytes, 0);
assert_eq!(outcome.render_attempts,
    outcome.sparse_attempts + outcome.full_attempts);
assert!(outcome.render_attempts <= 4);
assert!(target.completed_view().is_some());
// The test's explicit post-return texture copy/readback verifies these pixels.
// Reuse existing dense and exact untouched-pixel assertion implementations.
```

- [ ] Add `gpu_target_disabled_measurement_still_waits_for_completion` here, after Task 1's tracker-only disabled-measurement test. Use the actual renderer with measurement deliberately disabled and a narrow held fence-completion seam: checked counters may say `Complete`, but `completed_view` must stay hidden until the independent marker succeeds. A marker timeout or device-loss result still hides the view. Restore renderer environment controls within nextest's isolated test process.
- [ ] Add `gpu_target_linux_phase_limits_survive_recovery` and `gpu_target_absolute_deadline_spans_recovery` against the actual extracted render path with a narrow injectable clock/wait seam. Record requested timeouts: Linux keeps 1s fence and a fresh 1s callback even after 750ms fence elapsed; its counter phase gets one new 1s deadline, and repeated pending polls use decreasing remaining time. A completed overflow can start the next attempt with the same fixed phase limits, never a fifth attempt. Verify a Linux frame can complete with accumulated explicit waits exceeding 1s when every individual phase fits. Contrast the identical recovery work under an absolute GPU deadline: all waits consume the one caller deadline and no later attempt/fence is submitted once expired. Each phase timeout independently aborts without publishing or continuing recovery. Assert the actual path never requests more than 12s total Linux waits; the test clock avoids making unit tests sleep for twelve seconds. GPU pixel/recovery tests remain real-adapter tests, not replacements with a policy model.
- [ ] Cover actual production failure paths with a narrow test-only completion seam on this target/tracker: already-expired deadline submits nothing; pending counters persist until timeout; counter-map error, missing counter download and device-poll failure expose no view; overflow at attempts 1–4 ends with the existing exhausted-recovery error; a late callback cannot validate a reconstructed target. Tests still submit/use actual GPU targets where rendering is under test; inject only the external callback/poll outcome needed to make rare failures deterministic. Assert counters and history validity against the real renderer, not a parallel DTO model.
- [ ] Re-run existing bridge/XRGB history and FrameBuilder retained tests, including sparse readback correctness with row padding, one/two destination history repair, rejected identity, renderer replacement, large-shadow Quick Settings and root rebuild. Preserve `frame_retained_tests::assert_xrgb_pixels_eq`'s ≤16 differing bytes / maximum channel delta 1 and all exact untouched-byte assertions; use tighter existing assertions where present. GPU-only versus XRGB comparisons must verify the same scene after completion, not compare different animation instants.
- [ ] Run focused native tests, shell library/example checks, changed-crate clippy and formatting; self-review all old public bridge accessors/re-exports and renderer-state invalidation. Commit the complete target candidate. Controller reviews and validates that SHA on Fedora, including the current Linux `redraw_bench` and applicable compositor/headless regression coverage **only after its fixture is safe to run**. Preserve its existing metrics and thresholds; physical DRM is required if buffer-lifetime/presentation boundaries are affected.

## Task 3: Add the explicitly isolated common 1,000-action workload

**Files:** `shell/examples/shared_gpu_bench.rs`, `shell/examples/support/shell_gpu_fixture.rs`, explicit fixture-construction leaves in shared FrameBuilder/chrome only if the reviewed core seam still requires them, manifest/test scheduling registrations.

**Consumes:** `RetainedGpuTarget::render_until`, production `FrameBuilder::with_tree`, `WorkspaceTree::bootstrap_default`/`append_empty_workspace`, actual `widget_registry` and `dispatch_event`.

**Produces:** `shared_gpu_bench`, with 1,000 mandatory completed samples at 3840×2160, no window, no normal framebuffer readback, and an explicit fixture/version in its report.

- [ ] Build fixture `empty-workspace-chrome-v1`: three named empty workspaces (Home, Writing, Research), in-memory records, no collaborators/transport identity, no grants or app tabs, peer sync disabled, and fixed dark appearance/text scale from freshly seeded profile configuration. Use the actual root/chrome/sidebar/overview/Quick Settings construction. Do not call `FrameBuilder::new`/`bootstrap_demo`: it eagerly builds app tabs, including ambient Files/Terminal paths. No fake Notes, fake filesystem or substitute UI is needed for this deliberately scoped **shell-chrome** workload. It is not the populated Linux benchmark baseline or real-app qualification. Record normal/reduced motion choice explicitly and keep it identical on both hosts; a deterministic input sequence does not imply identical animation timing.
- [ ] Inject the actual controlled fonts before fixture construction/measurement: `ui::font_catalog::FontCatalog::controlled_fixture(root, locale)` and `ui::text_render::TextRenderer::with_font_catalog(&catalog)` exist. Thread that renderer through the reviewed FrameBuilder/text ownership seam; do not silently fall back to `TextRenderer::new` or system discovery. Record catalog-manifest identity, locale, font configuration, scale and the reviewed renderer-weight/typography-fixture commits. This plan does not implement or substitute those typography workstreams; if injection or their qualification is unfinished, report cross-host pixel qualification as pending even when a local GPU test passes.
- [ ] Before any runtime execution, audit fixture construction transitively: `FrameBuilder` appearance/settings initialization, `ChromeState` defaults, workspace-root creation and overlay constructors. The existing `with_tree` route avoids the live status poller but is not itself proof that every constructor is isolated. Use `RecordStore::in_memory`, no live status/provider polling, no notification/network/launcher/broker startup, no document opener installation with ambient authority, and explicit empty app construction. Read fixed configuration only under this fixture's private XDG root. If the core cutover does not provide the needed leaf options, extract the narrow actual initialization into explicit fixture inputs; do not add a mock success service or a broad host framework. If a constructor still requires native accounts/files/secrets, stop this fixture's runtime gate and report that precise prerequisite rather than guessing an authority API.
- [ ] Keep the wrapper but add an isolated parent invocation that validates and seeds private XDG directories and a temporary granted-test-data directory before constructing widgets; argv/profile data carries those paths. Preserve HOME only for toolchain resolution; no widget receives HOME/cwd as implicit data authority. Use a fresh environment with renderer overrides cleared or explicitly recorded, including `VELLO_*`/wgpu variables which the existing wrapper does not clear. Verify no active app tabs/services can be created by the two chosen controls. Fixture-construction tests inspect the actual tree/host and provider-start paths, not just the presence of temp environment variables. Ambient Files remains out of this fixture until its explicit provider contract exists.
- [ ] Extract/copy only the small real input driver from `redraw_bench::dispatch_click`: cursor move, left press, left release through FrameBuilder. Require a button event to be handled; hover alone does not count. Alternate exactly `topbar_tray_pill`, `topbar_overview_button` for 1,000 samples. Resolve fresh registry geometry each sample and fail on absent/zero-area targets. Use `relayout` then `paint_with_damage`, not `paint_settled` or a manually constructed scene. Warm up with one completed initial production frame and record that count; no dropping slow samples.
- [ ] Assert the actions change the actual shared host: snapshot `(frame.chrome().overlays.quick_settings, frame.chrome().overlays.overview)` around dispatch and require a changed pair, then require a nonempty produced scene and a fresh FramePaint revision. Add an untimed regression that renders settled before/after poses through this same fixture and verifies changed actual pixels while checking retained/full equivalence. A new revision alone, a handled hover, or repeatedly repainting an unchanged scene cannot satisfy the 1,000-action gate. Keep the same fixture/action validation and font inputs on Linux and Metal.

```rust
const EXTENT: (u32, u32) = (3840, 2160);
const SAMPLES: usize = 1000;
const ACTIONS: [&str; 2] = ["topbar_tray_pill", "topbar_overview_button"];
for index in 0..SAMPLES {
    let started = Instant::now();
    // Resolve ACTIONS[index % 2] from the live registry and dispatch the
    // real cursor/press/release sequence, failing if neither button is handled.
    frame.relayout();
    let paint = frame.paint_with_damage();
    let stats = target.render_until(&paint.scene, SceneDamage {
        revision: paint.revision,
        previous_revision: paint.previous_revision,
        regions: paint.damage_regions.as_deref(),
    }, started + Duration::from_secs(1))?;
    samples.push(started.elapsed()); // includes input, layout, render and recovery
    // Store stats after timing; emit logs after the loop, not per frame.
}
assert_eq!(samples.len(), SAMPLES);
```

- [ ] Add a separately invoked verification mode using the same fixture/action driver and actual GPU target, with retained/full pixel comparisons outside the timed run. Force fixed settled poses only in the oracle via the existing capture/test controls where necessary, and record those settings. Do not copy framebuffer pixels in the timed loop. The mandatory complete retained/full suite from Task 2 remains a prerequisite; the benchmark must print whether its separate oracle gate was executed and at what commit/backend, not fabricate a passed flag.
- [ ] Print/report fixture version, source commit and lockfile identity, build profile/target, OS, adapter name/backend/device type/driver, physical extent, warmup/sample count, mean/p50/p95/p99/min/max, actual full/sparse/recovery attempts, completion-marker count, and zero normal framebuffer copy/staging bytes. Report tracked target + Vello memory using existing `Renderer::memory_stats`; those counters are not total driver memory. Output existing GPU timestamp diagnostics only when requested/supported and match exact submission IDs; pending/unsupported/failed/missing samples stay explicit. CPU elapsed-through-fence is always the primary required completion metric. Add report schema fields with `unavailable` for allocation rate/RSS until measured through an actual allocator or host profiler; never report zero as a substitute. Controller can attach host profiler measurements outside the uninstrumented baseline and label overhead separately.
- [ ] Add example tests for exact 1,000 sample accounting, nearest-rank percentiles and any failed sample causing failure, plus an actual shorter GPU exercise of the production driver under isolated state. Test target availability/handled clicks and nonempty scene encoding. Do not run 1,000 GPU samples inside every unit test. Register actual GPU test names in nextest scheduling; test and benchmark commands require a real adapter and cannot silently return success when unavailable.
- [ ] Run the oracle gate, native common benchmark and focused example tests under isolation. Self-review source for accidental `paint`, `paint_into_xrgb8888`, texture-to-buffer copy or `wait_indefinitely` in the normal GPU-only call closure; pair this with runtime copy/allocation counters and code review rather than trusting a text search alone. Commit. Controller reviews and runs the same common benchmark/oracle on Fedora at that SHA. Record separate results without applying Linux's XRGB threshold to this new boundary or asserting an unmeasured Mac window cadence target.

## Execution commands and handoff

The controller resolves the exact reviewed core SHA before dispatch and keeps this sequence serialized. Adapt only test namespace moves against final code; record discovered names/counts. Run from the repository root with the required host-specific build environment. **Use the root graph for Vello**: although it is excluded from workspace membership, it is a selectable local patched dependency. Read-only `cargo pkgid --manifest-path Cargo.toml --package vello --locked --offline` resolved `path+file:///Users/xaver/dev/qos-macos-shell/vendor/vello#0.9.0` during this revision. `docs/performance-integration-validation.md` records root `cargo test -p ui -p shell -p vello --lib ... --locked --no-run` and 118 passing vendor renderer tests. This supports the command route; it is not a claim the new tests have run.

Root `Cargo.toml` patches **vello, vello_encoding and vello_shaders** to their actual local forks, and root `Cargo.lock` pins those local packages plus wgpu 29.0.3. Root nextest configuration supplies `ci`, process isolation and current scheduling; run the vendor selection serially and independently from other GPU jobs. Do not use `--manifest-path vendor/vello/Cargo.toml`: its standalone lock is ignored by `.gitignore`, currently lacks the manifest's direct `smallvec` dependency, and points encoding/shaders at registry packages. Root manifest patches are not inherited by that standalone package. No separate vendor lock/config setup or regeneration is needed or authorized by this plan, and root `Cargo.lock` must remain unchanged.

Run discovery before each corresponding selection and save JSON/name counts in the ignored task ledger. Verify the Vello test binary resolves to `vendor/vello`, that every named new test is discovered, and that GPU tests fail when an adapter is missing. A discovery/build failure is a prerequisite failure, not permission to switch silently to the stale standalone lock or drop `--locked`/`ci`. These exact commands select the root graph and configuration:

```text
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest list --manifest-path Cargo.toml -p vello --lib --locked --config-file .config/nextest.toml --profile ci --message-format json -E 'test(checked_bump_)'
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --manifest-path Cargo.toml -p vello --lib --locked --config-file .config/nextest.toml --profile ci --test-threads 1 --no-tests fail -E 'test(checked_bump_)'
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --manifest-path Cargo.toml -p vello --lib --locked --config-file .config/nextest.toml --profile ci --test-threads 1 --no-tests fail
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --manifest-path Cargo.toml -p shell --lib --locked --config-file .config/nextest.toml --profile ci --no-tests fail -E 'test(gpu_target_) or test(retained_tests::) or test(xrgb_history::)'
cargo check -p shell --lib --examples --locked
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run --manifest-path Cargo.toml -p shell --example shared_gpu_bench --locked --config-file .config/nextest.toml --profile ci --no-tests fail
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo run -p shell --example shared_gpu_bench --release --locked -- --verify
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo run -p shell --example shared_gpu_bench --release --locked
```

The new example must implement `--verify`; this is a proposed CLI above. Select its test target explicitly: `--all-targets` would also build the not-yet-ported Mac main and unrelated integration targets. Native main/all-target qualification belongs to its actual runner producer; Linux all-target regression gates remain. Complete formatting/clippy for changed files/crates, and the relevant shell library regressions in addition to these focus filters. Never treat focus filters as the full workspace matrix. Existing `redraw_bench` has now been run safely at4898888d after both explicit hosted Terminal and owned Files fixtures landed; preserve those inputs and distinguish its populated workload from the new chrome-only fixture.

Each author's report contains source SHA, root lockfile/configuration hashes before and after validation, exact commands/environment and discovered test counts, adapter identity, failures/soft skips, preserved Linux API/test inventory and benchmark reports. Controller performs task-scoped review, exact-SHA Fedora checks and required CI before acceptance; no source author modifies Fedora or runs a second writer concurrently. A rejected candidate is fixed by its owner and revalidated at a new explicit SHA.

## Decisions and limits to carry into review

Timeout revision recorded 2026-09-13: preserve Linux one-second per-phase limits; replace indefinite normal-frame counter settlement with one bounded one-second phase per attempt. The derived ≤12s explicit-wait bound is not a wall-clock guarantee and does not resolve the existing Fedora fence-timeout regression. GPU-only callers retain an independent absolute-deadline contract. Require focused timeout compatibility tests and actual Linux regression/benchmark evidence before acceptance; do not enlarge phase limits to obtain a pass.

Controller decision recorded 2026-09-13: the common workload may use explicit in-memory workspace/records and no ambient app construction because the specified boundary requires the real FrameBuilder/renderer and 1,000 deterministic 4K actions, not populated app fixtures. `empty-workspace-chrome-v1` is approved with real changed-frame assertions and identical workload on Linux/Metal. It does not replace populated Linux `redraw_bench`, real Notes/Files cadence or provider qualification. Controlled FontCatalog injection plus renderer-weight/fixture plans are required before claiming cross-host pixel equivalence; current ambient defaults are insufficient.

The proposed common fixture intentionally measures empty-workspace chrome, not the populated demo or real Notes/Files. A populated common-app workload requires the explicit factory/service integration candidate and a new labelled fixture baseline; it cannot be smuggled in through ambient constructors. The current Linux benchmark's workload/thresholds remain intact, with execution gated by safe fixture construction. The completion marker is a measurable extra queue operation, not a Vello render attempt; a future Vello submission-index exposure can remove it only with equivalent completion tests and separate evidence. Deadline enforcement cannot preempt arbitrary driver/CPU encoding stalls. Allocation rate, RSS and display delivery need real instrumentation beyond existing Vello counters. Native surface/blit/lifecycle/cadence and full app/service/distribution acceptance remain subsequent work.
