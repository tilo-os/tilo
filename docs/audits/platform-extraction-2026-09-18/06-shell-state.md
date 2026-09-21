# A06 — Shell state, runtime host, network and collaboration platform extraction

## Scope and method

Scope: `crates/session/shell/src/shell/**`, `crates/session/shell/src/collab/**`, `crates/session/shell/src/network/**`, `crates/session/shell/src/runtime_host/**`, root `crates/session/shell/src/*.rs`, and `crates/session/shell/tests/**`. Compositor, sandbox, security, portal, and binary backends are covered separately. The audit examines target guards, Unix and descriptor APIs, FFI, host directories, subprocesses, services, hardware, and adapter consumers. It is static source analysis rather than cross-platform runtime qualification.

## Destination alignment

Read `docs/architecture/platform-services.md`: this is a proposed design, not implemented APIs. Every platform boundary recommended below means focused implementation crates under `crates/system` or `crates/runtime`, reused directly by shell/headless consumers. Apps may receive the corresponding `ui::platform` facade; shell services must not acquire a UI-renderer dependency to use those mechanisms. Existing authorization, native handles, durability and consent remain owned by the service.

## Actionable findings

**Source-anchor note:** Verified prefix repairs identify current moved paths where
available. Any `:line` suffix remains the original 2026-09-18 audit coordinate; it
is historical evidence and does not assert that the cited symbol is at that line
in the current tree.

### A06-01 — File-search fallback directly invokes Linux desktop opener
- **Evidence:** `crates/session/shell/src/shell/search/providers/files.rs:1285` routes directories and unclaimed files to `spawn_open`; `:1323` launches `xdg-open` without a platform guard and discards spawn errors.
- **Classification/impact:** leaked Linux desktop assumption in app-facing search; alternate hosts lack a native default-open route and failure is not surfaced.
- **Boundary/migration:** introduce semantic `open_external_document`/`reveal_directory` capability, retaining Tilo document resolution first. Platform chooses native opener and returns a typed outcome; search uses existing `report_open_failure`.
- **Acceptance:** fake opener proves exact path forwarding, directories and unclaimed types; installed Tilo document handler remains preferred; unavailable opener yields a visible failure. Verify Linux and macOS native routes.

### A06-02 — Host-app catalogue and launch model expose desktop-entry backend
- **Evidence:** `crates/session/shell/src/shell/host_apps.rs:23`, `:191`, `:202`, `:213`, `:260`, `:430` expose DesktopEntry/Flatpak source and scan caches; Linux launch is guarded at `:66` with non-Linux handling at `:70`. `crates/session/shell/src/shell/search/providers/host_applications.rs` consumes this catalogue.
- **Classification/impact:** Linux backend plus leaked catalogue representation; native application discovery cannot be supplied simply by swapping launch.
- **Boundary/migration:** retain desktop-entry scanning and Flatpak invocation behind `InstalledApplications` (stable app identity, display metadata, supported documents, launch intent, generation), preserving placement/activation logic in shell. Do not turn a native unsupported launch into unconfined arbitrary execution.
- **Acceptance:** injected catalogue drives search/cache invalidation and document launch; Linux Flatpak matching and launch authority tests remain; unsupported targets report unavailable explicitly.

### A06-03 — Process launch, service descriptors and runtime directories mixed with shell policy
- **Evidence:** `crates/session/shell/src/shell/launcher.rs:141` proc start identity; `:188` Unix gate; `:198` fcntl descriptor inheritance; `:901`, `:1012` secure files; `:1055` XDG runtime selection with explicit macOS branch `:1060` and `/run/user` fallback `:1069`; `:1328` packaged helper paths; `:1665` Linux launch versus `:1654` non-Linux implementation. `crates/session/shell/src/shell/room_records.rs:964` derives app data from `XDG_DATA_HOME` launch environment.
- **Classification/impact:** deliberate Linux sandbox launch backend with OS assumptions in general lifecycle; Unix is shared by macOS/Linux, not Linux-only.
- **Boundary/migration:** extract authenticated process identity, service attachment, private runtime allocation and constrained launch into platform implementation. Keep workspace capability decisions, app registration and placement in shell. Return semantic app-data root instead of reading launch-environment key. Reuse launcher service handshake rather than inventing a parallel launch path.
- **Acceptance:** preserve launch-registration ordering, one-shot handoff, CLOEXEC ownership and PID-reuse checks; native host route remains explicitly a trusted development route, never a sandbox bypass.

### A06-04 — General brokers bind Unix transport and peer identity directly
- **Evidence:** `crates/session/shell/src/identity_broker.rs:4`, `:50`, `:383`; `crates/session/shell/src/shell/tool_intents.rs:4`, `:104`, `:112`, `:226`; `crates/session/shell/src/shell/collab_broker_capnp.rs:398`, `:9175`, `:9205`, `:9330`, `:9375` bind sockets, peek protocol bytes with libc and authenticate peer credentials/PID. `crates/session/shell/src/shell/desktop_continuation.rs:59`, `:83`, `:160` secure Unix continuation gate (Linux module guard in `crates/session/shell/src/shell/mod.rs:38`).
- **Classification/impact:** deliberate authenticated IPC backend mixed with portable identity/action/collaboration policy.
- **Boundary/migration:** move listener lifecycle, safe socket creation, byte transport and authenticated local-peer evidence to platform; expose verified peer/session identity and stream abstraction. Retain protocol limits and authorization in brokers. Lack of authoritative peer PID must fail closed, not accept claimed PID.
- **Acceptance:** same-user/other-user, stale path, marker race, spoofed identity, shutdown and descriptor-lifetime cases run on each backend; native continuation only advertised where implemented.

### A06-05 — Native runtime host already has a Unix seam, but daemon and MCP bypass it
- **Evidence:** `crates/session/shell/src/runtime_host.rs:46`, `:105` use `unix`; `crates/session/shell/src/runtime_host/unix.rs:1`, `:17`, `:55`, `:96` implement shared Unix permission and inherited-descriptor edge. `crates/session/shell/src/runtime_host/daemon.rs:14`, `:67`, `:147`, `:197`, `:209`, `:257`, `:266`, `:380` still directly use sockets, setsid/pre-exec, profile paths and Unix file types. `crates/session/shell/src/runtime_host/mcp_process.rs:9`, `:142`, `:259` directly use Unix flags, XDG profile layout and child launch.
- **Classification/impact:** intentional compositor-free native host; existing boundary is incomplete. macOS profile selection exists (`daemon.rs:69`), so do not label all runtime host Linux-only.
- **Boundary/migration:** lift existing `unix` implementation into platform services for private profiles, local service transport, daemon lifecycle, subprocess supervision and descriptor transfer; inject profile-directory mapping into MCP. Keep Session, manifests and RPC policy portable.
- **Acceptance:** existing daemon/MCP process tests retain startup locking, stale socket handling, profile symlink refusal, exec descriptor ownership and child cleanup; run equivalent platform integration suites.

### A06-06 — Secure persistence primitives duplicated across domain modules
- **Evidence:** `crates/session/shell/src/device_fleet.rs:422` flock; `crates/session/shell/src/identity_registry.rs:199` O_NOFOLLOW/O_CLOEXEC; `crates/session/shell/src/priority_contacts.rs:197` ownership and flags; `crates/session/shell/src/shell/identity_provisioning.rs:274`, `:295`, `:561`, `:675`; `crates/session/shell/src/shell/session_policy.rs:98`; `crates/session/shell/src/shell/collab_mls_authority.rs:168`, `:194`; `crates/session/shell/src/shell/device_fabric_runtime/mod.rs:4647`; `crates/session/shell/src/shell/network.rs:3116`; `crates/session/shell/src/network/behaviour.rs:184`, `:205` private permissions.
- **Classification/impact:** leaked Unix filesystem mechanics, security-sensitive; extracting without equivalent guarantees risks weakening identity/secret integrity.
- **Boundary/migration:** semantic private directory/file, no-follow regular-file open, owner-only atomic replace and exclusive lock capabilities. Reuse existing runtime-host private-file implementation as a starting point, but reconcile distinct checks before consolidation. Keep document formats, key rotation and authorization in original modules.
- **Acceptance:** symlink substitution, wrong owner, permissive mode, malformed existing data, concurrent writers and crash persistence remain rejected/preserved as appropriate on each backend. Do not replace with plain `fs::write` or cosmetic permission stubs.

### A06-07 — Onboarding UI owns privileged enrollment processes and mount discovery
- **Evidence:** `crates/session/shell/src/onboarding_view.rs:15` Unix CommandExt; `:2089`–`:2110` process-group SIGTERM/SIGKILL cleanup; `:2235` system identity root; `:2241` pkexec privileged helper; `:2954` picker scope assumes HOME, USER, `/run/media` and `/media`. `:134`, `:151` secure status-file read.
- **Classification/impact:** leaked Linux integration inside user-facing UI; process cancellation and removable-volume choices vary by OS. `/proc` at `:3070` is test-only, unlike actual signal cleanup.
- **Boundary/migration:** `IdentityEnrollmentSession` with start/progress/cancel and attested requester identity; platform privileged implementation owns helper and process group. `UserLocations` supplies home/removable roots. UI retains state machine, consent and narrow picker policy.
- **Acceptance:** cancel terminates descendants and reaps leader; progress spoofing rejected; unsupported enrollment disabled honestly; removable-media scopes provided by each host without broadening filesystem authority.

### A06-08 — First boot, config and machine identity encode Linux deployment locations
- **Evidence:** `crates/session/shell/src/firstboot.rs:44`, `:49` passwd lookup; `:116`, `:117`, `:453` XDG/HOME config; `:135`, `:373` gsettings host-preference import; `crates/session/shell/src/config.rs:112` `/etc/tilo-desktop/desktop.toml`; `crates/session/shell/src/network/behaviour.rs:35`, `:39`, `:60` system device-key/legacy roots; `crates/session/shell/src/runtime_host/daemon.rs:67` host-specific profile selection.
- **Classification/impact:** some deliberate Fedora defaults, some policy-level location/import assumptions. Existing `dirs` and `runtime_identity` callers should be reused instead of duplicating platform mapping.
- **Boundary/migration:** central system/user/state/cache/identity locations and `HostPreferenceImport`. Keep `/var` and `/etc` defaults in Fedora deployment backend, preserving existing-path migration and explicit env overrides. Keep gsettings parsing in Linux importer.
- **Acceptance:** Linux paths and one-time migration unchanged; native profile uses native locations; absent home, corrupt identity and unavailable preferences preserve documented outcomes.

### A06-09 — Filesystem publication helpers hard-coded into state and network actor
- **Evidence:** `crates/session/shell/src/shell/state.rs:27`, `:122` runs `/usr/bin/fs`; `crates/session/shell/src/network/actor.rs:2833`, `:2839`, `:2846`, `:2905`, `:2911`, `:2916` use publisher plus `/var/cache/quick-fs` with override support.
- **Classification/impact:** leaked deployment/process mechanism in workspace authority and peer network handling; same service knowledge duplicated.
- **Boundary/migration:** `WorkspaceAuthorityPublisher`/filesystem service client accepts typed publication and owns helper discovery, cache location and supervised execution. Policy remains in state/actor and continues to await/record failures.
- **Acceptance:** exact authority payload and revocation sent once, failed publication surfaced, host override precedence preserved; portable fake verifies semantics without installed `/usr/bin/fs`.

### A06-10 — Nearby transfer and cache space use OS filesystem implementation
- **Evidence:** `crates/session/shell/src/shell/nearby_transfer.rs:63`, `:77`, `:153`, `:170` directory descriptors, proc fd path and Linux openat2; explicit macOS branches `:81`, `:592`, `:1529`, `:1599` already exist. `crates/session/shell/src/shell/network.rs:327` uses statvfs for available bytes.
- **Classification/impact:** security-critical OS backend embedded in portable transfer domain. `not(macos)` is broader than Linux for proc-fd path.
- **Boundary/migration:** preserve `SecureNearbySource`/`SecureNearbyDestination` semantic model; extract capability-relative no-symlink reads and create-new installation to platform with Linux/macOS implementations. Add storage capacity query; no path-reopen fallback.
- **Acceptance:** rename-race and symlink traversal tests exercise production capability path; destination remains descriptor-anchored; quotas, folder archive rules and no-overwrite behavior unchanged; unsupported backend fails explicitly.

### A06-11 — Topbar performs Linux process liveness and private file IPC during draw
- **Evidence:** `crates/session/shell/src/shell/topbar_chrome.rs:10`, `:258`, `:3009`–`:3021` reads XDG clock activity, checks Unix uid/mode and `/proc/<pid>` while refreshing media/status.
- **Classification/impact:** clear Linux leak in rendering/chrome; native host can have valid clock producer but no `/proc`.
- **Boundary/migration:** authenticated `ClockActivitySource` supplies activity snapshots and producer liveness outside draw; keep pill/toast state in chrome. If retaining file protocol, owner verification and process identity are platform responsibilities.
- **Acceptance:** live/dead producer, invalid owner/mode/oversized payload and notification dedup verified with fake source; native clocks activity shown without procfs.

### A06-12 — Hardware capabilities and device class selected in shell
- **Evidence:** `crates/session/shell/src/shell/quick_settings/actions.rs:43`, `:57`, `:70` checks Linux binaries and execute bits, non-Linux defaults unavailable. `crates/session/shell/src/shell/device_fabric_runtime/mod.rs:38`, `:1398` chooses Linux uinput provider; `:7289` reads DMI chassis and defaults Desktop. `crates/session/shell/src/shell/physical_gamepad.rs:12`, `:59`, `:70`, `:82` stores Linux catalog/source types directly.
- **Classification/impact:** leaked backend selection despite existing `os_actions` service boundary; native hardware availability and laptop/tablet classification can be wrong/unavailable.
- **Boundary/migration:** obtain capability snapshot, device class and physical/virtual controller factory from platform. Reuse os_actions dispatcher, peripheral broker and associated portable event/identity types. Keep fabric leases/focus/consent in shell.
- **Acceptance:** unavailable/permission-denied/hotplug are distinct, native capability drives UI, forwarding revokes on focus/disconnect, backend selection contains no Linux concrete types in fabric policy.

### A06-13 — Remote audio and capture should reuse existing target adapter seams
- **Evidence:** `crates/session/shell/src/shell/remote_audio.rs:870`, `:940` directly spawn pw-record/pw-play. `crates/session/shell/src/shell/remote_capture/adapter.rs:41` existing adapter trait; `:549`, `:627`, `:683`, `:701`, `:812`, `:820`, `:835` O_TMPFILE, V4L2, PipeWire and system discovery. `crates/session/shell/src/shell/remote_capture/source.rs:36` source trait; `:70`, `:230`, `:258`, `:273`, `:408`, `:416`, `:429`, `:451` FFmpeg/PipeWire/scanner, sysfs and anonymous Linux spools.
- **Classification/impact:** deliberate Linux media backend partly isolated, audio lifecycle still mixes backend and protocol. Local endpoint enum explicitly uses PipeWire node/owned fd; these must remain private and never become CRDT/wire identity.
- **Boundary/migration:** move production trait implementations and process supervision to platform; add audio capture/playback equivalent with typed format/chunks. Preserve source availability, fresh app approval, endpoint nonserialization, bounded queues and cancellation.
- **Acceptance:** no process or descriptor leaks after stop; unsupported hardware unavailable; queue pressure/consent revocation unchanged; native backend emits identical protocol chunks without exposing native handles.

### A06-14 — Desktop service adapters live with notification/media/Bluetooth models
- **Evidence:** `crates/session/shell/src/notifications.rs:901`, `:943`, `:997`, `:1007`, `:1061`, `:1260` Linux-guarded D-Bus notifications and Flatpak `/proc`/cgroup identity; `crates/session/shell/src/shell/mpris.rs:327`, `:365`, `:514`, `:834` direct D-Bus media sessions; `crates/session/shell/src/bluetooth.rs:20`, `:25`, `:115` Linux registration but BlueZ request/reply types in shared prompt model.
- **Classification/impact:** intentional Linux desktop protocol backends with model coupling. Notification guarded code is not evidence of native compile failure; MPRIS needs a native media-session counterpart.
- **Boundary/migration:** notification ingress/action responder with verified app identity; media-session discovery/control; pairing prompt semantic events. Keep continuity, notification policy, consent and UI queues in shell; move D-Bus/BlueZ/Flatpak verification to platform without weakening identity provenance.
- **Acceptance:** spoofed notifier/player rejected; authenticated action routes unchanged; pairing timeout/cancel preserved; native host gets capability absence or native implementation.

### A06-15 — Projection, catalogue and Windows compatibility select packaged helpers
- **Evidence:** `crates/session/shell/src/shell/projection.rs:37` existing ProjectionBackend, `:76`, `:93` packaged projector helper; `crates/session/shell/src/shell/catalogue.rs:124`, `:236`, `:352` catalogue binary discovery/execution; `crates/session/shell/src/shell/windows_compat.rs:16`, `:25`, `:122`, `:132`, `:391` Bwrap/Libvirt/RAIL plus system install root and effective UID.
- **Classification/impact:** intentional Linux-host integrations and deployment defaults. “Windows compatibility” is a Linux-host VM feature, not a Windows native backend.
- **Boundary/migration:** relocate SystemProjectionBackend intact behind existing trait. Encapsulate trusted helper resolution and catalogue execution. Inject compatibility runtime factory using existing service broker interfaces; retain workspace authorities and grants in shell.
- **Acceptance:** exact helper arguments/timeout/errors preserved; no arbitrary path replacement in privileged flows; unsupported host capability explicit; VM isolation, portal documents and audio scope unchanged.

### A06-16 — Compositor/network graphics are deliberate platform backend
- **Evidence:** `crates/session/shell/src/shell/foreign.rs:86`, `:291`, `:341` Linux-guarded DMA-BUF mmap/ioctl; `crates/session/shell/src/network/wayland.rs:546` Wayland FD surface; `crates/session/shell/src/network/surface_tracker.rs:47` guarded surface tracking; `crates/session/shell/src/collab/flatpak_share.rs:8`; `crates/session/shell/src/collab/encoder/mod.rs:169`, `:193`, `:218` frame/encoder contract carries DMA-BUF raw FDs; `crates/session/shell/src/collab/encoder/vaapi.rs:172`, `:603` render nodes and DRM PRIME; `crates/session/shell/src/collab/encoder/nvenc.rs:85` NVIDIA probe. `crates/session/shell/src/network/decoder.rs:244` FFmpeg HEVC feature backend.
- **Classification/impact:** deliberate native graphics backend; retain Linux implementations behind boundary, do not mechanically rewrite colorimetry, HDR metadata, codecs or portable OpenH264 as platform logic. FFmpeg itself is cross-platform; packaging/codec availability is capability-dependent.
- **Boundary/migration:** preserve Encoder trait and portable frames, encapsulate platform-native frame handle variant and import/encode lifetime in backend; extract Wayland surface event adapter. Keep Iroh protocol/replica, input firewall policy, color transforms and GPU-independent metadata in domain layer.
- **Acceptance:** Linux zero-copy and fd ownership preserved; native handle never serialized; capability negotiation/SDR fallback maintained; HDR/codec tests select supported runner deliberately.

### A06-17 — Entry points are already appropriate platform composition roots
- **Evidence:** `crates/session/shell/src/main.rs:3`, `:449`, `:454` Linux/macOS/unsupported dispatch; `crates/session/shell/src/linux_main.rs:251`, `:514` Linux keyring/error and D-Bus startup; `crates/session/shell/src/lib.rs:44` native library tests reuse app tests.
- **Classification/impact:** deliberate platform selection, not a leak to erase. Main's native test module is not production macOS implementation.
- **Boundary/migration:** keep minimal target entry points; have them construct platform adapters consumed by shared shell/runtime. Move low-level implementations, not all orchestration blindly. Coordinate with compositor and app-framework auditors.
- **Acceptance:** both entry points construct explicit capability sets; Linux boot/session order and native development-host behavior remain intact; unsupported target is explicit.

### A06-18 — Test/platform suite separation and pre-existing hardware skip constraints
- **Evidence:** `crates/session/shell/tests/interaction_test.rs:12` imports Linux Wayland/sandbox path; `crates/session/shell/tests/native_atspi_smoke.sh:13`, `:20`, `:31` D-Bus/systemd/process discovery; `crates/session/shell/tests/hevc_hardware_encode.rs:1`, `:13` feature/hardware-gated suite; `crates/session/shell/src/collab/encoder/vaapi.rs:1237` pre-existing ignored hardware test. Runtime process tests use Unix sockets, signals, `/usr/bin/true`, XDG fixtures (`crates/session/shell/src/runtime_host/daemon_process_tests.rs:6`, `:57`, `:77`; `mcp_process_tests.rs:24`, `:286`). Test candidates appendix lists all matched files.
- **Classification/impact:** test-only host constraints, not production leaks. Existing self-skips/ignore are not proof of successful hardware verification and conflict with the stated fail-if-unobservable covenant when such tests are claimed as coverage.
- **Boundary/migration:** separate portable state/protocol suites from Linux compositor, transport, accessibility and hardware integration suites. Gate test targets by supported backend and arrange dedicated capability-provisioned runners; selected verification must fail on missing subject. Do not newly ignore or weaken tests.
- **Acceptance:** portable library and test targets compile on both supported hosts; required Linux `cargo check -p shell --tests --locked` and scoped suites pass on equipped Linux runner; hardware jobs assert real backend use rather than silently using software or returning early. No checks run in this audit.

## False positives / retain portable logic

- `crates/session/shell/src/shell/system_monitor.rs:11` uses cross-platform sysinfo; it may move under a semantic telemetry service for architecture, but no Linux-specific bug is established.
- `history_narrative.rs:99`, `:156` invokes optional Ollama: process supervision/model availability boundary, not inherently an OS-specific executable.
- `device_fabric_secrets.rs` is portable cryptography; os_actions WifiSecurity does not itself imply secret-storage platform coupling.
- Search extension filters containing `exe`, `dmg`, `flatpak`, strings in malformed-input tests, `/tmp` fixture values, shader files and cross-platform wgpu/openh264 code are not extraction targets by themselves.
- Most Quick Settings operations already flow through `os_actions`; avoid copying those implementations back into a new shell-specific abstraction.
- No vendor subtree is present in this assigned scope.

## Migration ordering

First consolidate secure storage, local authenticated transport, process identity/lifetime and host location primitives (A06-03–08,10). Then replace direct UI/domain calls (A06-01,09,11,12). Move desktop and hardware adapters using existing traits (A06-13–16). Wire at existing entry points (A06-17) and split verification by actual backend (A06-18). All contracts above are proposals unless explicitly identified as existing; no assumption is made that a current platform crate already exposes them.

## Coverage appendix

The following is a per-file lexical candidate index, not additional findings. Anchors include production and tests; use the finding context above before extracting. Raw inventories contain all matching lines. Adjacent ownership: manifests/build script, underlying os_actions/identity/desktop-entry/compatibility services, compositor/security/portal implementations and app SDK internals are outside this report.

### Production/module candidates

- `crates/session/shell/src/bluetooth.rs:15` — 7 candidate lines; representative anchors 20, 22, 66, 115, 123.
- `crates/session/shell/src/collab/encoder/mod.rs:5` — 4 candidate lines; representative anchors 193, 216, 420.
- `crates/session/shell/src/collab/encoder/nvenc.rs:15` — 11 candidate lines; representative anchors 43, 44, 51, 52, 85.
- `crates/session/shell/src/collab/encoder/vaapi.rs:9` — 15 candidate lines; representative anchors 58, 60, 70, 72, 120.
- `crates/session/shell/src/collab/flatpak_share.rs:7` — 10 candidate lines; representative anchors 8, 26, 28, 84, 92.
- `crates/session/shell/src/collab/mod.rs:7` — 3 candidate lines; representative anchors 11, 15.
- `crates/session/shell/src/config.rs:9` — 3 candidate lines; representative anchors 46, 112.
- `crates/session/shell/src/device_fleet.rs:5` — 3 candidate lines; representative anchors 73, 422.
- `crates/session/shell/src/firstboot.rs:2` — 23 candidate lines; representative anchors 5, 19, 20, 40, 44.
- `crates/session/shell/src/identity_broker.rs:4` — 21 candidate lines; representative anchors 50, 85, 383, 400, 412.
- `crates/session/shell/src/identity_registry.rs:12` — 11 candidate lines; representative anchors 13, 198, 199, 230, 234.
- `crates/session/shell/src/lib.rs:44` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/linux_main.rs:251` — 2 candidate lines; representative anchors 514.
- `crates/session/shell/src/main.rs:3` — 5 candidate lines; representative anchors 6, 11, 449, 454.
- `crates/session/shell/src/network/actor.rs:2833` — 4 candidate lines; representative anchors 2846, 2905, 2916.
- `crates/session/shell/src/network/behaviour.rs:182` — 7 candidate lines; representative anchors 184, 204, 205, 211, 258.
- `crates/session/shell/src/network/input_firewall.rs:1184` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/network/invite.rs:111` — 2 candidate lines; representative anchors 549.
- `crates/session/shell/src/network/mod.rs:13` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/network/streaming.rs:297` — 8 candidate lines; representative anchors 489, 555, 644, 743, 774.
- `crates/session/shell/src/network/surface_tracker.rs:2` — 12 candidate lines; representative anchors 5, 7, 47, 75, 134.
- `crates/session/shell/src/network/wayland.rs:546` — 2 candidate lines; representative anchors 554.
- `crates/session/shell/src/notifications.rs:9` — 96 candidate lines; representative anchors 22, 28, 30, 33, 34.
- `crates/session/shell/src/onboarding.rs:211` — 2 candidate lines; representative anchors 222.
- `crates/session/shell/src/onboarding_view.rs:15` — 24 candidate lines; representative anchors 131, 133, 134, 148, 150.
- `crates/session/shell/src/priority_contacts.rs:197` — 3 candidate lines; representative anchors 200, 205.
- `crates/session/shell/src/runtime_host.rs:46` — 3 candidate lines; representative anchors 47, 105.
- `crates/session/shell/src/runtime_host/daemon.rs:14` — 18 candidate lines; representative anchors 16, 57, 67, 68, 69.
- `crates/session/shell/src/runtime_host/daemon_process_tests.rs:2` — 24 candidate lines; representative anchors 6, 13, 34, 48, 57.
- `crates/session/shell/src/runtime_host/mcp_process.rs:9` — 9 candidate lines; representative anchors 140, 142, 143, 144, 145.
- `crates/session/shell/src/runtime_host/mcp_process_tests.rs:21` — 19 candidate lines; representative anchors 24, 25, 26, 27, 153.
- `crates/session/shell/src/runtime_host/unix.rs:7` — 11 candidate lines; representative anchors 9, 22, 51, 52, 68.
- `crates/session/shell/src/shell/catalogue.rs:124` — 5 candidate lines; representative anchors 125, 236, 352, 517.
- `crates/session/shell/src/shell/collab_broker_capnp.rs:18` — 39 candidate lines; representative anchors 19, 20, 352, 398, 402.
- `crates/session/shell/src/shell/collab_mls_authority.rs:10` — 9 candidate lines; representative anchors 11, 168, 170, 192, 194.
- `crates/session/shell/src/shell/desktop_continuation.rs:9` — 11 candidate lines; representative anchors 10, 11, 41, 59, 83.
- `crates/session/shell/src/shell/device_fabric_runtime/mod.rs:9` — 3 candidate lines; representative anchors 4647, 7289.
- `crates/session/shell/src/shell/doc_open.rs:677` — 3 candidate lines; representative anchors 689, 724.
- `crates/session/shell/src/shell/file_picker_overlay.rs:178` — 2 candidate lines; representative anchors 182.
- `crates/session/shell/src/shell/foreign.rs:36` — 73 candidate lines; representative anchors 40, 42, 43, 45, 50.
- `crates/session/shell/src/shell/history_narrative.rs:99` — 2 candidate lines; representative anchors 156.
- `crates/session/shell/src/shell/host_apps.rs:59` — 37 candidate lines; representative anchors 66, 70, 87, 115, 191.
- `crates/session/shell/src/shell/hosted_inputs.rs:191` — 3 candidate lines; representative anchors 213, 353.
- `crates/session/shell/src/shell/identity_provisioning.rs:24` — 26 candidate lines; representative anchors 25, 274, 277, 285, 295.
- `crates/session/shell/src/shell/launcher.rs:15` — 118 candidate lines; representative anchors 17, 19, 24, 26, 27.
- `crates/session/shell/src/shell/mod.rs:38` — 4 candidate lines; representative anchors 123, 1101, 1105.
- `crates/session/shell/src/shell/mpris.rs:9` — 28 candidate lines; representative anchors 21, 327, 340, 365, 366.
- `crates/session/shell/src/shell/nearby_transfer.rs:11` — 68 candidate lines; representative anchors 12, 13, 27, 29, 31.
- `crates/session/shell/src/shell/network.rs:8` — 47 candidate lines; representative anchors 328, 331, 332, 698, 699.
- `crates/session/shell/src/shell/notes_app.rs:1260` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/shell/overlay_host/mod.rs:25968` — 7 candidate lines; representative anchors 25970, 26004, 26005, 26340, 26356.
- `crates/session/shell/src/shell/peer_windows.rs:667` — 4 candidate lines; representative anchors 670, 703, 705.
- `crates/session/shell/src/shell/persist.rs:91` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/shell/personal_continuity.rs:1311` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/shell/projection.rs:8` — 3 candidate lines; representative anchors 76, 93.
- `crates/session/shell/src/shell/quick_settings/actions.rs:43` — 6 candidate lines; representative anchors 52, 56, 69, 71, 152.
- `crates/session/shell/src/shell/realtime_session.rs:2379` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/shell/remote_audio.rs:870` — 2 candidate lines; representative anchors 940.
- `crates/session/shell/src/shell/remote_capture/adapter.rs:10` — 13 candidate lines; representative anchors 11, 12, 543, 549, 551.
- `crates/session/shell/src/shell/remote_capture/source.rs:12` — 13 candidate lines; representative anchors 13, 230, 258, 273, 408.
- `crates/session/shell/src/shell/render/xrgb_history.rs:29` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/shell/room_records.rs:958` — 2 candidate lines; representative anchors 964.
- `crates/session/shell/src/shell/search/files_index.rs:283` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/shell/search/providers/actions.rs:3227` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/shell/search/providers/applications.rs:744` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/shell/search/providers/files.rs:924` — 6 candidate lines; representative anchors 1323, 1715, 1726, 1733, 1969.
- `crates/session/shell/src/shell/session_audio_policy.rs:5` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/shell/session_policy.rs:96` — 2 candidate lines; representative anchors 98.
- `crates/session/shell/src/shell/state.rs:27` — 12 candidate lines; representative anchors 122, 204, 277, 354, 357.
- `crates/session/shell/src/shell/tab_documents.rs:384` — 4 candidate lines; representative anchors 418, 433, 465.
- `crates/session/shell/src/shell/test_control.rs:1` — 5 candidate lines; representative anchors 1577, 1585, 1608, 1612.
- `crates/session/shell/src/shell/tool_intents.rs:4` — 6 candidate lines; representative anchors 42, 104, 112, 226, 307.
- `crates/session/shell/src/shell/topbar_chrome.rs:9` — 6 candidate lines; representative anchors 10, 258, 3009, 3013, 3021.
- `crates/session/shell/src/shell/uri_open.rs:128` — 1 candidate lines; representative anchors .
- `crates/session/shell/src/shell/windows_compat.rs:122` — 3 candidate lines; representative anchors 391, 552.

### Integration test candidates

- `crates/session/shell/tests/advertised_globals.rs:22` — 2 candidate lines; representative anchors 57.
- `crates/session/shell/tests/apps_integration.rs:18` — 2 candidate lines; representative anchors 72.
- `crates/session/shell/tests/clipboard_e2e.rs:3` — 80 candidate lines; representative anchors 5, 26, 27, 28, 150.
- `crates/session/shell/tests/color_management_protocol.rs:2` — 4 candidate lines; representative anchors 18, 235, 304.
- `crates/session/shell/tests/compositor_test.rs:101` — 12 candidate lines; representative anchors 108, 172, 175, 176, 177.
- `crates/session/shell/tests/control_socket_test.rs:25` — 14 candidate lines; representative anchors 52, 116, 123, 147, 185.
- `crates/session/shell/tests/dmabuf_encode_path_integration.rs:8` — 10 candidate lines; representative anchors 48, 49, 53, 59, 71.
- `crates/session/shell/tests/dmabuf_feedback_protocol.rs:11` — 10 candidate lines; representative anchors 27, 121, 124, 125, 126.
- `crates/session/shell/tests/document_open_e2e.rs:206` — 6 candidate lines; representative anchors 261, 268, 381, 382, 409.
- `crates/session/shell/tests/flatpak_share_e2e.rs:3` — 1 candidate lines; representative anchors .
- `crates/session/shell/tests/flatpak_share_encoder_probe.rs:59` — 4 candidate lines; representative anchors 97, 126, 129.
- `crates/session/shell/tests/fractional_scale_protocol.rs:2` — 4 candidate lines; representative anchors 23, 204, 247.
- `crates/session/shell/tests/headless_examples_smoke.rs:54` — 1 candidate lines; representative anchors .
- `crates/session/shell/tests/identity_startup.rs:11` — 3 candidate lines; representative anchors 51, 54.
- `crates/session/shell/tests/interaction_test.rs:201` — 2 candidate lines; representative anchors 310.
- `crates/session/shell/tests/it.rs:3` — 1 candidate lines; representative anchors .
- `crates/session/shell/tests/keyboard_focus_wire.rs:14` — 2 candidate lines; representative anchors 173.
- `crates/session/shell/tests/leak_probe.rs:21` — 1 candidate lines; representative anchors .
- `crates/session/shell/tests/native_atspi_smoke.sh:1` — 11 candidate lines; representative anchors 13, 14, 20, 31, 49.
- `crates/session/shell/tests/nvenc_hardware_encode.rs:2` — 1 candidate lines; representative anchors .
- `crates/session/shell/tests/opening_configure_carries_the_tab_size.rs:22` — 2 candidate lines; representative anchors 142.
- `crates/session/shell/tests/portal_file_chooser_e2e.rs:32` — 2 candidate lines; representative anchors 191.
- `crates/session/shell/tests/portal_prompt_drain_e2e.rs:47` — 2 candidate lines; representative anchors 76.
- `crates/session/shell/tests/portal_test.rs:5` — 4 candidate lines; representative anchors 32, 115, 1038.
- `crates/session/shell/tests/runtime_host.rs:9` — 2 candidate lines; representative anchors 16.
- `crates/session/shell/tests/sandbox_check.rs:19` — 11 candidate lines; representative anchors 36, 43, 53, 55, 56.
- `crates/session/shell/tests/server_side_decorations.rs:20` — 2 candidate lines; representative anchors 132.
- `crates/session/shell/tests/session_lock_e2e.rs:31` — 16 candidate lines; representative anchors 45, 46, 59, 69, 70.
- `crates/session/shell/tests/tilo_e2e_test.rs:57` — 1 candidate lines; representative anchors .
- `crates/session/shell/tests/two_netns_share.rs:92` — 2 candidate lines; representative anchors 98.
- `crates/session/shell/tests/wayland_test.rs:118` — 1 candidate lines; representative anchors .
- `crates/session/shell/tests/xdg_activation.rs:9` — 3 candidate lines; representative anchors 159, 372.
