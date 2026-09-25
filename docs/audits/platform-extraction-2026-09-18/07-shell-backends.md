# A07 — shell backends and integration boundaries

## Scope and method

Scope: `crates/session/shell/src/{compositor,wayland,sandbox,security,portal,bin}/`, the shell manifest, and shell-owned examples and packaging. Root shell state, integration tests, and application-framework internals are covered separately. The audit examines target guards, Unix and FFI use, process commands, native dependencies, host paths, device interfaces, protocols, and target scripts. Intentional Linux-only backends remain platform backends rather than portability defects.

## Destination alignment

Read `docs/architecture/platform-services.md`. It is proposed, not implemented. In this report “platform framework” means focused non-rendering `crates/system` / `crates/runtime` services, optionally surfaced to apps through `ui::platform`. It does **not** mean moving compositor security ownership or native Wayland/portal backends into UI. Shell consumes service crates directly; the UI/app framework internals are outside this audit. Native backends retain authorization/enforcement authority. No framework-to-app dependency should be introduced.

**Source-anchor note:** Verified prefix repairs identify current moved paths where
available. Any `:line` suffix remains the original 2026-09-18 audit coordinate; it
is historical evidence and does not assert that the cited symbol is at that line
in the current tree.

## A07-01 — Extract Unix local transport from portable control/capture contracts

**Classification / priority:** leaked platform representation, medium; Linux control servers themselves are intentional backends.

**Evidence:** `crates/session/shell/src/bin/tilo-control.rs:13` imports UnixStream unconditionally, `:20` defaults to `/tmp/tilo.sock`, and `:645` installs SIGPIPE handling. `crates/session/shell/src/compositor/projection_capture.rs:9` imports UnixStream in an unconditionally exported module (`compositor/mod.rs:53`); its default profile/socket discovery at `projection_capture.rs:282` and `:291` uses XDG_RUNTIME_DIR. `portal/capture_broker.rs:10` imports Unix OwnedFd, and `:90` exposes PipeWireNode/FileDescriptor in LocalCaptureHandle although capture authorization is a target-local, semantic authority service. These work as Unix assumptions on macOS, but are not portable Windows contracts; no Windows build claim is made.

**Boundary:** local authenticated IPC endpoint/discovery and opaque local capture transport handles in the platform framework; semantic control JSON, capture lease, app/workspace/session binding and revocation stay shared. Preserve non-serializability of native handles. Reuse broker lease model and projector protocol; do not create another control protocol.

**Migration:** separate socket I/O from control parsing and frame encoding; supply an endpoint provider and transport implementation; move native handle variants under backend-private wrappers; preserve explicit endpoint overrides. Linux keeps Unix sockets and SIGPIPE behavior; other hosts either implement authenticated IPC or advertise unavailable.

**Acceptance:** semantic CLI round trips and capture lease revocation tests remain identical across injected transports; Unix disconnect/broken pipe does not hang; native handles cannot enter collaborative serialization; unavailable transport fails explicitly.

## A07-02 — Host paths and private persistence are embedded in shared portal services

**Classification / priority:** extract shared-service OS assumptions, high for maintaining privacy guarantees.

**Evidence:** `portal/mod.rs:183` imports Unix permission APIs and `:561` resolves private state from XDG_STATE_HOME / dirs::state_dir / XDG_RUNTIME_DIR; `:571` writes authority using 0700/0600 and directory fsync. Similar private stores: `secret.rs:11`, `permission_store.rs:14`, `background.rs:12`, `lockdown.rs:10`, `desktop_services.rs:13`, `screencast.rs:279`, `remote_desktop.rs:424`. `desktop_services.rs:946` discovers document-portal mount using XDG_RUNTIME_DIR or `/run/user/<uid>`; `:954` parses `/proc/self/status` and falls back to UID 0. This module is exported without Linux guard. `picker_queue.rs:146` chooses Documents from XDG then `~/Documents`, and `:171` reads `~/.config/user-dirs.dirs` directly (does not honor an alternate XDG_CONFIG_HOME there). `doc_store.rs:43` specifies `/run/tilo/documents` as the sandbox ABI.

**Impact:** shared consent/service models inherit Linux storage and export semantics; simply replacing directory strings would lose access-control and authority durability requirements. Linux document-portal paths and sandbox mount paths are deliberate backend contracts, not generic document identities.

**Boundary:** platform private-state storage (atomic durable replacement plus user-only access), user directories, and authorized document export resolution. Reuse existing `atomic-file` and platform directory facilities where their contracts suffice; verify private permissions/ACL guarantees rather than assume them. Existing `DocumentStore` / `FilePickerSpawner` and desktop service traits are useful seams.

**Migration:** keep consent records, prompt queue and capability decisions shared; inject private store and document-export service; move procfs/runtime mount resolution into Linux portal implementation; resolve Documents through host directory capability. Keep fail-closed behavior instead of substituting shared temp storage or treating `/dev/null` as a real store.

**Acceptance:** Linux export canonicalization still rejects outside paths/symlink escapes; missing private storage cannot persist authority; per-host private ACL/mode tests; non-default XDG config path honored by Linux Documents provider; macOS/Windows shared picker policy does not probe procfs.

## A07-03 — Retain Wayland/DRM compositor as explicit Linux platform backend

**Classification:** intentional platform implementation; extraction should preserve it, not mechanically generalize every syscall.

**Evidence:** Linux guard inventory `compositor/mod.rs:9`; `backend/mod.rs:13` selects DRM and `:19` selects Smithay Winit. `native.rs:212` authenticates peers with SO_PEERCRED, `:236` compares procfs namespaces, and `:1072` publishes XDG session display. `backend/drm/runtime.rs:2485` enumerates sysfs DRM; `gpu_import.rs:80` uses device IDs and `:320` inspects GPU fdinfo. `render_node.rs:59` exposes libc::dev_t but actual node probing is Linux-gated (`:106`, `:116`, `:135`). `selection.rs:1000` creates pipe2 transfer channels. Accessibility uses accesskit_unix (`accessibility.rs:91`). `xwayland.rs:378`, `:513`, `:572` integrate setxkbmap, X11 sockets and xwayland-satellite. `osk_service.rs:210` authenticates peer credentials and `:246` checks procfs descendants. `bridge.rs:14` and floating/control services are Unix server endpoints behind Linux module guards.

**Boundary:** compositor platform backend crate/modules own Smithay, Wayland protocols, DRM/KMS, libinput/libseat, EGL/dmabuf imports, Xwayland process management and AT-SPI adapter. Reuse `Backend`/`Output` in `backend/mod.rs`, existing scene/frame/pacing policy, `PasswordVerifier`, and selection policy seams. Do not move app framework rendering. `wayland/color_management.rs` deliberately mixes guarded protocol adapter and portable color policy; `wayland/edid.rs` is byte parsing, not hardware I/O.

**Migration:** isolate target dependencies and native handle types; inject semantic output/input/session events into shared shell state. Move only hardware discovery/execution into backend; leave pure policy modules (`drm/policy.rs`, geometry, pacing, EDID parsing) shared. Preserve trusted-client attribution and fail-closed namespaces. `backend/mod.rs:15` already compiles pure DRM policy on non-Linux via path alias—prefer an explicitly named policy module eventually.

**Acceptance:** Linux headless interaction/control and DRM hardware integration retain peer authentication, buffer compatibility and lock suppression; shared policy tests require no devices; non-Linux artifacts need not link Smithay/libseat/PipeWire. All extracted control/capture capabilities remain discoverable through existing semantic actions.

## A07-04 — Keep sandbox enforcement Linux-specific; eliminate misleading duplicate construction APIs

**Classification / priority:** intentional security backend plus unused platform leakage, high migration sensitivity.

**Evidence:** `sandbox/mod.rs:5` documents launcher as sole production enforcement owner and `:16` documents profile as unused by launch paths. Linux-gated `landlock_shim.rs:363` execs the checked program; `seccomp.rs:79` chooses x86_64/aarch64/riscv64 and `:111` installs syscall rules. `profile.rs:181` has fixed runtime identity and `:249` binds runtime audio/device grants. In contrast publicly exported `security/container.rs:19` constructs bwrap without platform guard, with Fedora filesystem layout (`:27`), GPU/sysfs mounts (`:42`), fixed UID runtime fallback (`:63`) and predictable app logs (`:122`). Search found no `Container` caller in shell; dead/legacy status should be confirmed across external consumers before removal.

**Boundary:** semantic launch request/capability policy belongs shared; authoritative Linux bwrap/seccomp/Landlock compilation belongs launcher/platform Linux. Reuse launcher, do not promote `Container` or unused profile into a second enforcement mechanism.

**Migration:** confirm repo-wide/public API consumers; remove or clearly backend-gate unused container/profile APIs, keeping needed pure tests. Preserve live `__sandbox-exec` entry and one launch authority. Unsupported targets must fail closed rather than launch without equivalent enforcement. Kernel/architecture capability reporting must distinguish enforced, unsupported and intentionally unavailable.

**Acceptance:** production launch tests prove granted/denied resource behavior through actual launcher; no new unsandboxed fallback; Linux architecture tests compile filters for supported architectures; no generic API promises a sandbox merely because bwrap argv was constructed.

## A07-05 — Host service implementations should plug into existing semantic traits

**Classification / priority:** deliberate Linux services intermixed with shared policy, medium.

**Evidence:** `lock_auth.rs:37` defines PasswordVerifier; Linux greetd implementation is gated (`:50`), helper invocation uses `/usr/libexec/tilo-unlock-helper` (`:107`, `:150`), non-Linux factory returns UnavailableVerifier (`:253`). `portal/print.rs:105` already defines PrintSpooler; Linux LpSpooler (`:165`, `:212`) runs lp. `portal/desktop_services.rs:48`, `:123`, `:178`, `:422`, `:598`, `:738` contain Account/AppChooser/DynamicLauncher/Email/Screenshot/Wallpaper service traits. `compositor/capture.rs:982`, `:997`, `:1150`, `:1165` directly runs xdg-open. `compositor/audio_capture.rs:55`, `:76`, `:211` shells out to pw-record/wpctl and `:167` kills its Unix process group. These latter compositor modules are correctly Linux-gated, not current macOS execution leaks.

**Boundary:** platform services for local authentication, print submission, open/reveal output, and permission-scoped recording/audio source selection. Keep freedesktop method names and D-Bus encoding only in Linux adapters. Reuse existing traits rather than duplicating policy in a new facade.

**Migration:** relocate concrete implementations/dependency selection behind platform factories; route screenshot/recording completion open actions through a desktop-open service; keep lock controller and print/capture consent decisions shared. Native auth unavailable is safer than an improvised unlock implementation.

**Acceptance:** injected failures are explicit, authentication never accepts on backend failure, helpers remain bounded and secrets stay off argv; cancellation reaps audio process groups; printing/recording uses same grants and lock capture suppression after extraction.

## A07-06 — Portal protocol is Linux; capture/control authority is not

**Classification:** intentional backend requiring a careful split, medium.

**Evidence:** `portal/mod.rs:1` declares xdg-desktop-portal implementation and `:54` onward applies Linux/test guards to many adapters. `screencast_pipewire.rs:39` and `:64` expose PipewireStreamRequest/Info and `:414` connects via XDG_RUNTIME_DIR; protocol-facing node IDs are appropriate here. `clipboard.rs:4`, `input_capture.rs:15`, `remote_desktop.rs:33` transport Unix fd/EIS resources. Linux-specific account/app chooser/background/camera/dynamic launcher/email/inhibit/lockdown/notification/permission store/screencast/screenshot/secret/settings/USB/wallpaper backend files belong together (appendix gives all paths). Not every cfg(test) compiled protocol model should become a generic platform API.

**Boundary:** leave zbus interfaces, FD transfer, EIS and PipeWire provisioning in Linux platform portal backend; keep PromptQueue/PortalContext consent policy, CaptureBroker lease logic, picker queue and semantic service traits host-neutral. Reuse ScreenCastStreamProvisioner but avoid requiring every host's capture API to return PipeWire node IDs.

**Migration:** split backend-private wire DTOs from semantic requests/results; inject native transport producers; carry verified caller identity into the same capability flow. Maintain unavailable provisioner distinction from test mocks.

**Acceptance:** native Linux portal conformance and adversarial caller tests pass; non-Linux core policy compiles independently of D-Bus/Unix FDs; unavailable sources never advertise synthetic production streams; capture session close/revocation invalidates backend resources.

## A07-07 — Cargo and native bundle boundaries

**Classification:** build/deployment platform selection; retain target-specific scripts, tighten feature ownership when extracting.

**Evidence:** `shell/Cargo.toml:18` defaults hdr-decode, `:171` declares native ffmpeg-next under features; Linux native dependencies are grouped at `:286` (Smithay/PipeWire/seccompiler/Landlock/accesskit_unix/reis/greeter/compat-service). Unconditional zbus/libc/xattr and service dependencies reflect remaining Unix/Linux reachability, not proof each library is intrinsically nonportable. `shell/build-macos.sh:4` explicitly checks Darwin, `:12` builds no-default-features, `:14` assembles .app, `:29` ad-hoc signs. `compositor/macos.rs:35` prepares a largely ordinary hosted session and `:91` uses dirs::data_local_dir; this is a macOS runner, not a second Linux compositor. `shell/README.md:47` states its limits. `desktop.toml` is portable theme/collaboration data.

**Boundary / migration:** move dependencies with concrete platform implementations; use a deliberate Linux-desktop vs hosted-native capability composition, preserving hdr codec features as media capabilities rather than treating all ffmpeg as Linux-only. Retain macOS bundle script in deployment tooling; do not put codesign or Info.plist generation in runtime platform APIs. Keep macOS session construction/state orchestration shared where appropriate without broadening its sandbox claims.

**Acceptance:** Linux default and relevant feature combinations plus macOS no-default-features retain lockfile; dependency graph of hosted build excludes Linux native backends; platform capability availability matches README and user-visible errors. No build was performed in this audit.

## A07-08 — Test tools/examples carry intentional native prerequisites

**Classification:** test/tool constraints, separate from production portability.

**Evidence:** `bin/click_test_client.rs:1` and `bin/tilo-loadclient.rs:1` gate Wayland clients to Linux; `bin/shots/driver.rs:24` gates compositor state dependency. `bin/ux-bench.rs:2` is macOS runner and `:76` sets isolated XDG directories for fixtures. `examples/runtime-mcp-inspect.rs:48` uses Unix exec; `examples/pairing_e2e.rs:124` invokes sudo. Native backend unit tests use Linux devices, fork or /bin/sh; examples' `/tmp` output help and fixture URI strings are not production path bugs. `lock_auth.rs:387` Unix permission test imports are an additional non-Windows test constraint.

**Migration / acceptance:** label platform-specific binaries/tests and preserve Linux native integration suites alongside host-neutral parser/policy tests; use existing backend fakes only where subject is policy. Real hardware/enforcement tests must fail explicitly when selected without their prerequisites, never silently skip or replace production assertions with mock tautologies. Keep headless shots cross-host where wgpu supports it. No mandatory whole-test-suite portability promise should be inferred from a macOS shell-bin-only build.

## Candidate coverage appendix

The following files matched the broad search. First lexical matches are navigation anchors rather than independent issues; findings above distinguish platform implementations, pure tests or policy, and ownership leaks.

- `crates/session/shell/src/bin/click_test_client.rs:1`
- `crates/session/shell/src/bin/qos-runtime-mcp.rs:46`
- `crates/session/shell/src/bin/shots/diff.rs:278`
- `crates/session/shell/src/bin/shots/driver.rs:20`
- `crates/session/shell/src/bin/shots/main.rs:204`
- `crates/session/shell/src/bin/shots/mode.rs:427`
- `crates/session/shell/src/bin/shots/scenario.rs:886`
- `crates/session/shell/src/bin/tilo-control.rs:6`
- `crates/session/shell/src/bin/tilo-loadclient.rs:1`
- `crates/session/shell/src/bin/ux-bench.rs:2`
- `crates/session/shell/src/bin/ux_bench_support/mod.rs:14`
- `crates/session/shell/src/bin/ux_bench_support/tests.rs:54`
- `crates/session/shell/src/compositor/accessibility.rs:91`
- `crates/session/shell/src/compositor/activation.rs:97`
- `crates/session/shell/src/compositor/audio_capture.rs:12`
- `crates/session/shell/src/compositor/backend/drm/devices.rs:11`
- `crates/session/shell/src/compositor/backend/drm/gestures.rs:207`
- `crates/session/shell/src/compositor/backend/drm/hotplug.rs:146`
- `crates/session/shell/src/compositor/backend/drm/input_config.rs:494`
- `crates/session/shell/src/compositor/backend/drm/mod.rs:765`
- `crates/session/shell/src/compositor/backend/drm/outputs.rs:133`
- `crates/session/shell/src/compositor/backend/drm/runtime.rs:11`
- `crates/session/shell/src/compositor/backend/drm/scale.rs:112`
- `crates/session/shell/src/compositor/backend/drm/scanout.rs:10`
- `crates/session/shell/src/compositor/backend/drm/seat_diag.rs:41`
- `crates/session/shell/src/compositor/backend/drm/session.rs:183`
- `crates/session/shell/src/compositor/backend/headless.rs:175`
- `crates/session/shell/src/compositor/backend/mod.rs:13`
- `crates/session/shell/src/compositor/backend/winit.rs:591`
- `crates/session/shell/src/compositor/bridge.rs:9`
- `crates/session/shell/src/compositor/capture.rs:17`
- `crates/session/shell/src/compositor/client_input.rs:1480`
- `crates/session/shell/src/compositor/dev_env.rs:52`
- `crates/session/shell/src/compositor/drag_feedback.rs:239`
- `crates/session/shell/src/compositor/floating_service.rs:11`
- `crates/session/shell/src/compositor/gpu_import.rs:79`
- `crates/session/shell/src/compositor/hosted.rs:230`
- `crates/session/shell/src/compositor/hosted_test_control.rs:27`
- `crates/session/shell/src/compositor/idle.rs:327`
- `crates/session/shell/src/compositor/lock_auth.rs:17`
- `crates/session/shell/src/compositor/lock_controller.rs:269`
- `crates/session/shell/src/compositor/lock_screen.rs:783`
- `crates/session/shell/src/compositor/metrics.rs:529`
- `crates/session/shell/src/compositor/mod.rs:9`
- `crates/session/shell/src/compositor/mp4_recording.rs:322`
- `crates/session/shell/src/compositor/native.rs:15`
- `crates/session/shell/src/compositor/osk_service.rs:10`
- `crates/session/shell/src/compositor/output_globals.rs:271`
- `crates/session/shell/src/compositor/pacing.rs:230`
- `crates/session/shell/src/compositor/projection_capture.rs:9`
- `crates/session/shell/src/compositor/remote_seat.rs:287`
- `crates/session/shell/src/compositor/render_node.rs:20`
- `crates/session/shell/src/compositor/selection.rs:22`
- `crates/session/shell/src/compositor/state.rs:214`
- `crates/session/shell/src/compositor/surface_outputs.rs:107`
- `crates/session/shell/src/compositor/surface_readback.rs:222`
- `crates/session/shell/src/compositor/switch_control.rs:139`
- `crates/session/shell/src/compositor/xwayland.rs:11`
- `crates/session/shell/src/portal/account.rs:5`
- `crates/session/shell/src/portal/app_chooser.rs:3`
- `crates/session/shell/src/portal/auth.rs:201`
- `crates/session/shell/src/portal/background.rs:12`
- `crates/session/shell/src/portal/camera.rs:19`
- `crates/session/shell/src/portal/capture_broker.rs:10`
- `crates/session/shell/src/portal/clipboard.rs:4`
- `crates/session/shell/src/portal/desktop_services.rs:12`
- `crates/session/shell/src/portal/doc_store.rs:202`
- `crates/session/shell/src/portal/dynamic_launcher.rs:3`
- `crates/session/shell/src/portal/email.rs:2`
- `crates/session/shell/src/portal/file_chooser.rs:26`
- `crates/session/shell/src/portal/file_picker_spawner.rs:298`
- `crates/session/shell/src/portal/global_shortcuts.rs:9`
- `crates/session/shell/src/portal/inhibit.rs:8`
- `crates/session/shell/src/portal/input_capture.rs:12`
- `crates/session/shell/src/portal/lifecycle.rs:460`
- `crates/session/shell/src/portal/lockdown.rs:10`
- `crates/session/shell/src/portal/mod.rs:51`
- `crates/session/shell/src/portal/notification.rs:12`
- `crates/session/shell/src/portal/permission_store.rs:10`
- `crates/session/shell/src/portal/picker_queue.rs:143`
- `crates/session/shell/src/portal/print.rs:31`
- `crates/session/shell/src/portal/remote_desktop.rs:32`
- `crates/session/shell/src/portal/screencast.rs:27`
- `crates/session/shell/src/portal/screencast_pipewire.rs:22`
- `crates/session/shell/src/portal/screenshot.rs:3`
- `crates/session/shell/src/portal/secret.rs:11`
- `crates/session/shell/src/portal/settings.rs:2`
- `crates/session/shell/src/portal/usb.rs:9`
- `crates/session/shell/src/portal/wallpaper.rs:2`
- `crates/session/shell/src/sandbox/landlock_shim.rs:40`
- `crates/session/shell/src/sandbox/mod.rs:25`
- `crates/session/shell/src/sandbox/profile.rs:25`
- `crates/session/shell/src/sandbox/seccomp.rs:111`
- `crates/session/shell/src/security/container.rs:3`
- `crates/session/shell/src/wayland/color_management.rs:27`
- `crates/session/shell/src/wayland/edid.rs:324`
- `crates/session/shell/src/wayland/mod.rs:11`

Summary: eight grouped findings. Adjacent shell callers, launcher enforcement, media and projector services, runtime transport, and OS packaging require coordinated migration.
