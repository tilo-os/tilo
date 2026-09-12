# Direct macOS Shell Library Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. One integration implementer owns this complete cutover, followed by one task-scoped spec/quality review and controller-owned native/Fedora validation.

**Goal:** Make `cargo check -p shell --lib --locked` succeed natively on macOS using the real WorkspaceTree, chrome, FrameBuilder, collaboration and app-host code, while preserving Linux execution and portable test coverage.

**Spec:** docs/superpowers/specs/2026-09-12-macos-shell-port-design.md and docs/superpowers/specs/2026-09-12-macos-app-capability-map.md.

**Prerequisite:** Complete production wake integration at its reviewed commit. Font handle/weight and fixed-fixture plans are independently required for final visual qualification; they do not block source/dependency cutover, and this task must not alter their source scope or expected PNGs.

## Global Constraints

- “Use target-specific dependencies and module gates for Linux and macOS implementations; features select capabilities, not the operating system.”
- “Preserve Linux CLI defaults and commands.”
- “Keep Linux defaults and HDR behavior.”
- “Reuse WorkspaceTree, shell chrome, actions and FrameBuilder as the scene producer.”
- “Missing required terminal services must fail explicitly rather than silently launching an unconstrained host shell.”
- “A test may be Linux-only because it exercises a Linux facility; a portable action, document, collaboration or lifecycle test cannot be excluded just because its current module also imports Linux APIs.”
- “Benchmarks and tests must not load the user's real state or secrets.”
- No substitute shell UI, copied app business logic, fake service success, weakened test/metric/pixel assertions or new OS-selection Cargo feature. Unavailable native providers remain explicit errors pending their actual implementation.
- Keep all shared code in the existing shell crate. Presentation types/queue move to shared modules there, with old Linux paths re-exported. Do not create the optional qos-shell-presentation crate.
- Worktree /Users/user/dev/qos-macos-shell; Mac target /Users/user/dev/qos/target/macos-shell. Controller alone syncs committed SHAs to /home/parallels/qos-macos-shell and runs Fedora using /home/parallels/qos-target.
- Native FFmpeg prerequisite is installed as Homebrew ffmpeg@8 8.1.2 (not current ffmpeg 9): use `PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig` for Mac Cargo commands in this environment, without altering Fedora pkg-config selection. Verified libraries: avcodec 62.28.102, avformat 62.12.102, avutil 60.26.102, swscale 9.5.102. Do not hardcode this host path into portable source.
- Every runtime command uses .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py. Preserve nextest process isolation and GPU scheduling; serialize final goldens on an otherwise idle GPU.
- Treat the complete dependency/source cutover as one implementation candidate. Do not commit a manifest-only half, drop a common test module wholesale, or present a reduced library as the successful shell gate.

### Task 1: Compile and test the real shared shell library on both hosts

The architectural map below is the binding implementation map. It identifies exact existing owners and the private/shared boundaries to preserve; the implementer may adapt internal file splits to current source while retaining those contracts. This is an integration task, not verbatim patch transcription.

- [ ] Read the map and record the pre-change Linux dependency closure and test inventory. Use `cargo tree -p shell --target aarch64-apple-darwin --edges normal --locked`; inspect consolidated test registrations and count moved tests by name. Existing native shell check failure at wayland-sys (missing wayland-client.pc) is recorded in the foundation evidence. Do not install Wayland to mask it.
- [ ] Add or retain focused failing tests for descriptor transfer/cleanup on native Unix, absence of required Terminal service before any process spawn, and actual shared input wire-code mapping. Existing portable frame/queue/state/capability tests are regression tests that move with their production code; do not fabricate replacement test subjects.
- [ ] Apply Stage 1 dependency-library changes and Stage 2 shell source/manifest changes as one coherent implementation. Move whole existing implementations where possible, split only platform authority/protocol leaves, preserve public Linux re-exports, and maintain a concise old-to-new test applicability table in the task report.
- [ ] Type-check in dependency order using the native target directory: `cargo check -p launcher --lib --locked`, `cargo check -p terminal --lib --locked`, then **`cargo check -p shell --lib --locked` with normal default features**. Resolve actual compiler errors without stubbing required app behavior. If an ordinary cross-platform native library is missing (for example libavcodec), report the concrete host prerequisite for the controller to supply; do not remove Linux defaults to evade it.
- [ ] Execute focused tests for changed portable launcher policy, runtime-portal descriptor transport, Terminal required-service failure, and then `cargo nextest run -p shell --lib --locked --profile ci` through the isolated runner. Common shell tests must compile and execute on Mac; concrete Linux facility tests are target-selected with reasons. Require real adapters for GPU gates and record their identity. Retain documented pre-existing font/golden/GPU baseline failures as explicit unresolved evidence; do not mark attributable new failures accepted.
- [ ] Run library clippy for changed crates with `-D warnings`, format only changed files, inspect the resulting Darwin dependency tree for remaining Smithay/Wayland/PipeWire/seccompiler/Landlock/ush supervisor edges, and confirm no common test disappears. Shared schema, peer wire codes and capability vocabulary remain unchanged.
- [ ] Commit the complete native-validated candidate with task-scoped source and manifest files only. Detailed report goes to this plan's ignored SDD workspace, containing commands, counts, failures, dependency closure, moved-test map and unsupported provider inventory. Do not stage other queued plan documents or .superpowers artifacts.
- [ ] Controller mirrors that commit and runs the corresponding portable tests and `cargo check -p shell --all-targets --locked` on Fedora. Existing native/headless integration tests run there with their actual Linux providers. Record any pre-existing failure separately from newly introduced regressions and fix new failures before task acceptance.

The direct library cutover is accepted only after actual native shell compilation and task review plus Linux compatibility evidence. Native executable, real Notes/Files definitions and record/service integration, all-workspace host matrix and the three GPU benchmark boundaries remain required subsequent delivery. No library-only pass is goal completion.

## Architectural implementation map

## Recommendation

Make one coherent **shell library cutover**, including the necessary changes
to two existing dependencies in the same candidate if that reaches the native
gate sooner. The presentation-only extraction is optional staging, not a
prerequisite: its `FramePaint`, `OutputId` and presentation queue can remain in
shared modules of `shell` during this cutover. If it has already landed,
consume its re-exports rather than reversing it. Do not create
another shell-model/chrome/frame crate. Keep `shell::shell::state::WorkspaceTree`,
its shared handle, chrome, app
hosting, collaboration broker, persistence and the real `FrameBuilder` in the
existing `shell` package. Target-select concrete Linux implementations below
those owners. Preserve old Linux public paths through re-exports.

The next successful compilation gate must be `cargo check -p shell --lib
--locked` on Apple Darwin with the ordinary default features, followed by the
real portable shell test set on both hosts. A queue, scene DTO, replacement
widget demo, or a library containing only selected models is not this gate.

Three implementation work areas are sufficient; the first two can and should
land together when separate review would delay the actual shell build:

1. Make the launcher **library** and Terminal's descriptor client independent
   of Linux execution code, retaining their existing Linux behavior.
2. Cut the existing shell's mixed roots into shared implementation and Linux
   adapters together, including manifest target dependencies and real renderer
   tests. This is the actual shell `--lib` gate.
3. Add the application runner and canonical app factories/host services. This
   is necessary for the executable and real Notes/Files product milestone;
   stage 2 cannot claim those outcomes.

Stage 1's dependency edits are necessary because Cargo compiles those libraries
before shell can be type-checked, but this does not require a separate commit
or milestone. Stage 3 requires host lifecycle and provider semantics, not just
source relocation, and can begin against the same working candidate. The rows
below describe edits for one direct cutover, not an open-ended queue of new
intermediate crates. Native library compilation can precede complete native
provider qualification as long as missing capabilities remain explicit and
the app milestone is not claimed early.

## Observed dependency closure

| Shared entry | Current path into Linux code | Required boundary |
| --- | --- | --- |
| `compositor::frame::FrameBuilder` | `foreign::ForeignSurfaceWidget`, floating types/OSK ID in `compositor::state` | Move scene builder and floating presentation data into `shell`; retain foreign widget/store and isolate capture producer. |
| `FrameBuilder` → `build_nirios_overlay_host_with_chrome` → `ShellOverlayHost` | portal queues/services; notification bridge; power inhibitor registry; launcher; foreign store and input navigation | Keep real chrome; split each service's protocol/OS adapter from its actual shared state. |
| `shell::to_ui_tile` | unconditional `TerminalWidget`, Files, Browser, Word Processor constructors | Keep app implementations; remove Terminal's transitive supervisor build dependency and introduce required-service-aware hosted construction. |
| `shell::{state,search,catalogue,doc_open,uri_open,peer_windows,room_activity,collab_broker_capnp}` | `shell::launcher` → `launcher::{registry,manifest,services,appdir}` → unconditional Linux launcher modules | Keep real registry/manifest/services; target-gate launcher execution leaves. |
| `shell::launcher` | render-node pinning, dev environment, process spawning, `/proc` ancestry/start-time attestation | Preserve Linux execution/attestation in Linux adapter; common launch request/state/event flow calls selected execution authority. |
| `network::input_firewall`, `shell::peer_view` | `compositor::client_input` chord/key policy mixed with Smithay seat | Move actual policy and wire encoding; retain Linux seat delivery. |
| `network` root | unconditional `network::wayland` | Gate only this Smithay server module, not Iroh/broker/protocol/streaming/firewall. |
| `frame_retained_tests.rs` | `backend::drm::paint::{SceneDamage,VelloDrmBridge}` | Move entire real Vello bridge plus `xrgb_history` into shared renderer module; these files do not use DRM/Smithay objects. |
| `chrome_state`, `shell::notification` | root `notifications` mixes bridge with zbus server and `/proc` identity | Keep bridge/model shared; select freedesktop server and verified PID provider on Linux. |
| `overlay_host::session_inhibitors`, `status::SystemStatus` | power `InhibitRegistry` is pure std but exposed only under `service`; battery models share crate with logind/sysfs | Make pure registry available independently; select actuator at runner, retain battery models and policy. |
| Linux-only compatibility/session paths | `windows_compat` → `compat-service` → `windows-vm` → `launcher`; greeter, BlueZ, AT-SPI, PipeWire | Gate concrete root and dependency together; do not remove common launch metadata or chrome availability model. |

The existing `WorkspaceTree` itself depends on launcher catalogue queries in
resource/app selection. Treating it as a tiny pure CRDT crate would either keep
the dependency closure or require a larger redesign. That extraction is not
needed for this cutover.

## Stage 1: close existing dependency libraries

### Launcher

Prefer making `crates/packaging/launcher`'s existing library honest about target
support over copying its registry/schema into a new crate. Shell uses
`registry`, `manifest`, `services`, `appdir` and some capability policy; their
implementations use ordinary data/files/Unix descriptors, not Linux kernel
objects. Keep them compiled on Darwin and Linux. Keep `profile` and pure
capability decisions where required by this closure.

The subtle edge is `launcher::manifest`: `default_seccomp_profile` and
`MinimalManifest::seccomp_profile` call
`seccomp::{SeccompProfile,resolve_profile}`. Do **not** gate all of `seccomp`.
Split its existing `SeccompProfile`, `ProfileDecision`, entitlement check and
profile-selection logic from `seccompiler` imports, syscall tables,
`host_target_arch`, BPF generation and staging. Preserve `launcher::seccomp`
as the public vocabulary path, with Linux-only compiler exports. Profile
metadata is portable; accepting metadata must not imply enforcement exists.

Target-select the actual launcher execution modules `run`, `sandbox`,
`landlock`, `landlock_shim` and their Linux helper closure (`dbus_proxy`,
`gpu`, `host` where they serve Linux launch). Keep
`hardening::policy_matches` and its pure helper closure shared:
`seccomp::entitled_to_dynamic_code` calls it. Do not gate `hardening` wholesale
unless that helper moves with the shared profile vocabulary. Move `seccompiler`
and `landlock` dependencies into the Linux target table. `netgate` contains
both ordinary Unix/socket policy and Linux execution context: preserve any
pure policy consumers rather than adding a `cfg` to the whole crate. No
Darwin API should pretend to construct a usable bwrap/seccomp plan.

This is a library-only target cut. The `qos-launcher` binary remains the real
Linux sandbox launcher. A later all-targets inventory must explicitly select
its binary/tests; a Darwin shell dependency build need not build that binary.

### Terminal and `ush`

`apps/terminal` imports only `ush::supervisor::recv_with_fd` from `ush`, but
the dependency compiles the whole supervisor, guest, mount and confinement
stack. Move the existing Unix send/receive descriptor implementation and its
round-trip test to `runtime-portal::fd_transport` (an existing SDK/runtime
owner), and re-export `recv_with_fd` at `ush::supervisor` for Linux callers.
Terminal depends on `runtime-portal` for this transport and no longer depends
on `ush` merely to receive a PTY. Keep `ush`'s Linux service and enforcement
unchanged. Native qualification must check ancillary-buffer alignment, Darwin
`msghdr` field widths, close-on-exec and owned-descriptor cleanup on malformed
or truncated messages; this is an actual IPC implementation, not a byte mock.

**Do not stop at the dependency move.** `TerminalBackend::new` currently
chooses `BestEffort` when namespace/service probes fail; Darwin would take that
route and run the login shell. The hosted factory must instead accept an
explicit host process-service context, reject an absent required `ush`
capability before constructing the widget, and never re-enter the ambient
`TerminalBackend::new` route. Preserve existing Linux standalone behavior and
the mandatory service route. Make the shell's hosted construction path
fallible and retain a real retryable launch error when the required provider
is unavailable. No invented macOS PTY success provider, unconstrained shell
fallback, or Terminal removal from the source tree is justified by compilation.

The user-facing catalogue can report Terminal as unavailable with its precise
required-service reason until the Mac process provider is qualified. This
follows the approved capability map and is not a claim that Terminal works.

## Stage 2: one real shell library cutover

### Shared scene, foreign content and input

Use the following proposed owners inside `shell/src/shell`:

| Proposed owner | Exact existing implementation to move | Linux compatibility |
| --- | --- | --- |
| `frame.rs` and `frame_retained_tests.rs` | Whole `compositor/frame.rs`, including `FramePaint` unless already extracted, plus its sibling tests | `compositor::frame` re-exports the shared owner. |
| `render/paint.rs`, `render/xrgb_history.rs` | Whole `compositor/backend/drm/paint.rs` and `xrgb_history.rs` | Old DRM modules re-export all existing public items; keep `VelloDrmBridge` name for API compatibility in this move. |
| `floating.rs` | `OSK_APP_ID`, `FloatingTransferFrame`, `FloatingTransferSource`, `PendingFloatingToplevel`, `FloatingToplevel`, `FloatingToplevelEvent` and pure suspend/resume/return/clear/dismiss transitions from `state.rs` | `compositor::state` re-exports; real protocol owner/ack validation stays there. |
| `foreign.rs` with Linux child producer | Existing store/widget, publish/sequence/error/layout/presented state, tree reconciliation and label use | `compositor::foreign` re-exports; Linux producer retains WlBuffer, DMA-BUF, client/barrier and configure/frame callback operations. |
| `tab_binding.rs` | Existing whole tree placement/label/tab reconciliation implementation | Preserve authenticated launch placement lookup in `adopt_toplevel`; `compositor::tab_binding` re-exports. |
| `input_policy.rs` | Existing navigation, geometry, chord ownership, modifier conversion and peer wire encoding | Linux `client_input` imports/re-exports these and retains `ClientInput`/Smithay seat state. |

If skipping the presentation crate, move `OutputId` into the shared render
module and the unchanged `compositor/present.rs` queue into a shared sibling;
retain old-path re-exports. These need only `FramePaint`, Vello and std. There
is no need to wait for another package or introduce a second definition.

Moving `paint.rs` and `xrgb_history.rs` is particularly low risk: source
inspection found no Smithay, DRM handle, libc or platform-extension imports.
Their only internal sibling edge is `super::xrgb_history`; the paint test
module imports `FrameBuilder`. Keep the production retained/full renderer,
buffer history transactions, readback/swizzle, bounded allocation recovery,
GPU completion and diagnostics. Do not rewrite the oracle using a different
offscreen renderer. Renaming `VelloDrmBridge` can wait.

The foreign store split need not redesign its private entries or create a
generic image crate. Its `ForeignEntry` contains portable capture tokens and
ready pixel results, while `CaptureJob<T>` contains Smithay `Barrier` and
`Client`. Keep the real token ordering and rejection logic with the store;
place protocol workers/imports in a Linux child module, using narrow private
methods or child visibility instead of making entry fields public.
`capture_commit*`, `prepare_dmabuf_capture`, `AsyncDmabufCapture`,
`drain_completed_dmabuf_captures`, imports and configure/callback delivery are
Linux implementations. `publish_rgba`, snapshots, presentation latching,
layout, failure state, unmap/forget and tree reconciliation remain shared.
`forget` must still cancel producer work and reject late results. The store's
`paint_output_presentation_surface` already receives neutral scene/surface-ID/
rectangle/theme arguments; keep that API, and do not move `.audience` ownership
validation out of Linux State.

Input policy has one nontrivial portability correction: `evdev_keycode`
currently prefers `PhysicalKeyExtScancode::to_scancode`. macOS native scan
codes are not the peer protocol's Linux evdev values. Use an explicit shared
`KeyCode`→existing wire-code mapping; retain the Linux native scan-code path
where needed for identified keys, with logical fallback only under the current
documented rules. Test non-US physical-key semantics. The wire protocol is
unchanged. A pixel `SurfaceId` remains data, never permission to inject input.

### Portal and notification roots

Keep the existing `portal` namespace as a compatibility facade. It must not
unconditionally declare every D-Bus backend. Put shared definitions in ordinary
submodules and move server wiring into `portal::linux`; no new crate is needed
to make shell compile. Later reusable app-facing service interfaces belong
below SDK/runtime, not in a dependency from apps back to shell.

Required shared closure:

- From `portal/mod.rs`: `PortalCaller`, `CapabilityKind`, `GrantStoreHandle`,
  consent persistence, `PendingGrant`, `PromptOutcome`, `PromptResolver`,
  `PromptQueueHandle`, `with_scope` and their real capability decisions.
  Keep the same `WorkspaceTreeHandle`, grant expiry/scope and reply behavior.
- Existing `file_picker_spawner` request/filter/spawner/handle/options and
  `picker_queue` queue behavior. Split `PickerScope::for_user` ambient
  home/XDG discovery from the request's injected root policy; the Mac hosted
  Files/picker path must receive actual host grants.
- `desktop_services` request/result/trait/handle contracts, settings state,
  wallpaper records/cache/preview persistence and dynamic-launcher
  records/generation/snapshot used by the real application search provider.
  Splitting only screenshot and wallpaper types misses this search closure.
  Keep shared shell account/dialog/selection behavior; Linux document-portal
  path interpretation and execution adapters remain target-specific.
- `global_shortcuts` shared service/binding/consent state and accelerator
  policy used by `portal_dialog` and `overlay_host`; zvariant conversion,
  `#[interface]` dispatch, authenticated D-Bus lifecycle are Linux adapters.
- Notification `CloseReason`, `IncomingNotification`, `NotificationBridge`
  and its queue/action semantics remain shared. `NotificationService`, wire
  hints/values, bus emission and foreign-process identity discovery remain
  Linux-owned.

`PortalContext`, `CallerMap`, document-portal store/session construction,
authentication and `serve*` are presently bound to Linux package/D-Bus
delivery. Keep their existing exports on Linux. Do not install `No*` or
`NullFilePickerSpawner` defaults and call that a Mac provider implementation.
Existing `PortalResult` has Success/Cancelled/Unavailable/Failed; denial
currently lives in separate consent/capability paths. Preserve that behavior
and add typed provider distinctions where the approved SDK contract requires
them; do not falsely document a currently nonexistent Denied variant.

### Launcher/chrome coordination

Keep shared `shell::launcher` state/types/catalogue/event behavior and existing
call sites: installed app snapshots/default candidates, resource claims,
pending placement expiry, launch notices, identity projection and continuation
staging. Split execution behind one selected authority with explicit result
and instance/room context. A new provider interface is proposed work, not an
existing API.

The Linux implementation owns `build_launch_command`, spawn/watch/kill,
render-node parameters, sandbox and `.desktop` execution, `/proc` process
start/ancestry evidence and verified package/service launch matching. The
shared entry point must distinguish a registered embedded component launch
from installed Linux package execution. Unsupported Mac package execution is
a typed launch failure; it is not fabricated `Started`/`Exited` events and is
not a replacement for real embedded Notes/Files.

`collab_broker_capnp::authenticate_launcher_handoff` depends on this evidence.
Keep Linux same-user kernel credentials + PID/start-time attestation. A Mac
in-process factory gets host-issued instance-scoped authority through the
existing broker's trusted construction path; it must not claim a launcher PID
or bypass authorization because `/proc` is absent. Keep raw diagnostic broker
connections' current limited semantics. Neither a same UID nor an app-provided
package ID alone is a trusted launch binding.

`host_apps`/`tool_intents` Linux process-service endpoints can be selected at
the adapter, with shared search/result/intent types retained where chrome
uses them. `room_records` currently imports `launcher::appdir::{ROOMS_DIR,
SANDBOX_APP_DIR,sandbox_env}`: retain its Linux app-data import semantics, but
the Mac host's document root must be injected rather than treating a sandbox
path constant as a native storage root.

### Target manifest and source selection

Do this alongside the source split, not as a standalone manifest-only patch.
Target-select direct shell dependencies on Smithay, SCTK, Wayland client and
protocols, PipeWire, `seccompiler`, `landlock`, `accesskit_unix`, `reis`,
greeter, BlueZ, `compat-service` and `windows-vm` to Linux when their shared
consumers have been separated. The `launcher` library remains a common
dependency after stage 1. `zbus` is not inherently a Linux compile blocker;
remove only the shell's adapter dependency if the shared module split actually
eliminates its uses. Portable Unix APIs also are not Linux-only.

`os-actions` still unconditionally depends on `qos-bluez` and the default
power service feature. Moving the direct shell edges alone does not remove
those transitives. For the fastest honest compile slice, their ordinary Rust/
zbus libraries can remain in the Darwin compile closure while Linux-specific
actuation is not selected by the application runner; this is not native
Bluetooth/power support. If the candidate's dependency gate requires removing
those implementations entirely, split their existing shared models/traits
from `os-actions::bluez` and service providers in the same cutover, with
Linux-target dependencies and preserved Linux exports. Do not fake unavailable
hardware as successful reads or writes. This cleanup is not a hard native
compiler blocker merely because the protocols are freedesktop-specific.

Keep common runtime/collaboration/identity, Files/Browser/Word Processor,
Vello/wgpu/winit, models, codecs, persistence, `qdoc`, search and security
policy enabled. `ui` already target-selects Smithay clipboard/SCTK, but app
feature unification still enables `ui/portal` through Word Processor and
`ui/webview-adblock` through Browser. Audit those real feature closures.
Files' FUSE dependencies are dev-dependencies: they do not block shell's
library dependency build, but do block claiming Files' full Mac test matrix.

Preserve Linux default `hdr-decode`. FFmpeg is cross-platform native software,
not a Linux-only dependency: an ordinary Mac default build either supplies
the pinned libavcodec toolchain or introduces an actual Mac decoder provider
with equivalent declared capability. Running only `--no-default-features`
would not establish the intended shell build gate. VAAPI/NVENC implementations
remain selected by their supported provider/target, without hiding shared
HDR dispatch/policy tests.

Root source selection:

- Keep shared `shell`, `network`, `config`, `workspace`, `security`, identity,
  onboarding models/views, diagnostics, priority contacts and portable collab
  encoder/colorimetry/HDR modules.
- Linux-select compositor adapter modules: accessibility/seat/Wayland State,
  native/legacy hosted server runners, backend DRM/Wayland, capture producers,
  output globals, selection, lock/greeter/session ownership, Xwayland, OSK
  protocol services and their concrete closure. Keep extracted shared frame,
  policy and renderer owners independently compiled.
- Linux-select `network::wayland`, `collab::flatpak_share`, root `wayland`,
  root sandbox enforcement, Bluetooth service/prompt, and
  `shell::windows_compat`. Keep portable permission decisions outside sandbox
  enforcement. Gate only actual OS implementations of mixed test modules.
- Keep pure power inhibitor/model/policy APIs common by moving `inhibit`
  out of the power crate's `service` feature gate; retain the Linux service
  feature/default behavior. `status`, quick settings and media preferences
  keep actual shared UI/state. Their Linux pollers/actuators must be installed
  by the Linux runner, not spawned in a generic constructor on Mac.
- Move shared CLI mode/config parsing out of a Linux-only compositor root
  if the native entrypoint needs it. Retain Linux argument behavior and reject
  unsupported backend selections explicitly on Mac.

## Required native tests and API compatibility

Run each stage's dependency gates before shell, then shell on native Darwin
and Fedora at identical source. The enclosing workflow owns environment
isolation, toolchain prerequisites and VM transfer; no commands here assume a
VM name. Suggested gate commands are `cargo check -p launcher --lib --locked`,
`cargo check -p terminal --lib --locked`, then `cargo check -p shell --lib
--locked`, formatting, and package clippy. After target test selection, run
the actual portable shell library tests with nextest and the existing isolated
runner. Follow with Linux all-targets and existing integration/headless
regression tests. Inspect `cargo tree -p shell --target aarch64-apple-darwin
--edges normal --locked`: no Smithay/Wayland/PipeWire/seccompiler/Landlock/ush
supervisor via transitive dependencies. Do not infer compilation from this
tree alone.

Required preserved portable tests:

1. Actual WorkspaceTree placement/serialization/persistence, chrome actions,
   navigation, overlay host and widget registry tests. Test construction
   receives isolated app state and no uncontrolled terminal startup.
2. Real `FrameBuilder` tests and all portable retained tests, especially
   `native_4k_legacy_and_segmented_shell_frames_match_through_quick_settings_cycle`,
   `native_4k_retained_quick_settings_damage_covers_large_shadow_and_matches_dense`,
   `workspace_root_rebuild_forces_dense_boundary_and_keeps_live_topbar_pixels`,
   and both damage policy tests. Require a real wgpu adapter on both hosts;
   do not silently return success for missing GPU. Keep current bounded pixel
   comparison tolerance and report actual backend.
3. Existing render bridge/XRGB history tests; dense fallback on revision gaps,
   destination identity and invalidation after renderer replacement; bounded
   recovery and completion semantics. Their DRM directory name is not a reason
   to skip them on Mac.
4. Foreign store sequence/failure/layout/latch/unmap/token ordering and
   forget/late-result tests; floating suspend/resume/stale-ack/return policy;
   authenticated placement remains exercised through the shared policy.
5. Capability grant/revoke/expiry, consent queue cancellation, scoped picker,
   wallpaper/dynamic record persistence, notification queue/action and
   accelerator tests. Files/picker tests use granted temporary roots rather
   than ambient user home discovery.
6. Real catalogue/resource-claim/default selection, launch-event/placement
   lifecycle and continuation tests with an injected execution authority;
   assert absent required service fails before component construction and no
   fallback process is spawned. Protocol authentication tests must retain
   negative cases, not merely accept an in-process app ID.
7. Network protocol/identity/broker/replica/firewall/shared streaming tests.
   Split Linux State/SurfaceTracker fixtures out of mixed
   `network::{input_firewall,streaming}` test modules. Preserve equivalent
   protocol/policy assertions with their real portable owners.

Keep Smithay seat, WlBuffer/barrier, DRM/KMS/udev/libinput, Wayland protocol,
PipeWire/EIS, Linux process-confinement and logind/BlueZ service tests on Linux
with explicit facility reasons. Move pure tests with their implementation;
do not apply a whole-module `cfg(target_os = "linux")` to mixed tests just to
make nextest green. Keep a test applicability ledger with old/new test names
and exact counts. `autotests=false` plus `tests/it.rs` means integration test
selection must be addressed explicitly; dependency `--lib` success does not
prove `--all-targets`.

API invariants: one definition/re-export of moved types; same Linux
`compositor::{frame,foreign,tab_binding}` and DRM paint/history paths; preserve
`FramePaint`/OutputId ownership from presentation extraction if already landed,
or one shared owner inside shell if adopting the direct cut; unchanged
WorkspaceTree schema, peer wire codes, grant vocabulary, launcher manifest
schema and real Linux execution. Add fallible hosted construction rather than
silently changing existing functions' successful return meaning. No app or
`ui` dependency on shell.

## Stage 3 remains necessary, but must converge immediately

Stage 2 proves the real shell core compiles; it does not start a Mac shell
window. The next executable uses a main-thread winit ApplicationHandler,
drives the same FrameBuilder/chrome and production retained renderer, supplies
actual host input/clipboard/files/dialog/identity/broker services and consumes
the wake-scope lifetime cutover. The renderer's CPU readback test path does
not dictate a CPU-copy Mac presentation path, and `PresentPipe::spawn` does
not dictate ownership of AppKit surface presentation.

Real Notes is absent from the current shell dependencies: `to_ui_tile` imports
`shell::notes_app::NotesApp`, whereas the milestone requires
`apps/notes::NotesApp`. Add canonical application definitions/factories and
one registry shared by standalone/embedded/headless runners; preserve real
Notes workspace/page persistence, import and per-view semantics through the
host record service. Files already uses the real `FileExplorerWidget`, but
its hosted factory still needs injected roots/grants and document-opener
authority rather than ambient FUSE/home discovery. These are shared app-model
changes and require their own behavioral tests. Do not register the old Notes
widget under `dev.qos.notes` to close the milestone.

Native executable acceptance then includes actual Notes/Files UI, resize/IME,
clipboard/file operations, persistence and reopen with independent Views,
Linux–Mac collaboration and real accessibility. Keep Linux native compositor,
nested and headless execution intact. This final staging is necessary because
a native shell library and an app-provider implementation are different
deliverables; additional isolated presentation crates are not necessary.
