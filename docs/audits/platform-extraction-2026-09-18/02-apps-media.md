# A02 — Media, maps and weather app platform extraction audit

## Scope and method

Scope: `apps/{camera,image-editor,maps,music,photos,tube,video-editor,video_editor,video-player,weather}`. The audit covers manifests, Rust source, tests, examples, target guards, OS APIs, processes, storage, portals, hardware, media, GPU use, and native services. Application-framework internals, dependency internals, and OS deployment are outside its scope. Findings describe source coupling rather than observed cross-target failures.

## Destination architecture

Aligned with proposed `docs/architecture/platform-services.md`: expose semantic calls through `ui::platform`, implement host mechanisms in focused `crates/system` or `crates/runtime` services, and reuse existing media/portal/atomic-file services. New capabilities below are proposals, not assertions of existing API availability. Framework services must not import app crates; app state and domain policy stay in apps. Backend ownership must actually move, not remain in apps behind forwarding facades.

## Findings requiring extraction or adapter use

### A02-01 — Camera owns Linux V4L2 enumeration and capture (high)

- Evidence: `apps/camera/src/capture.rs:18` feature gate; `:79` scans `/dev` for `video<N>`; `:121` builds `v4l2src device=...`; `apps/camera/src/app.rs:2404` selects/opens devices from that list. `apps/camera/Cargo.toml:20` enables `live-capture` by default and `:53` supplies native GStreamer dependencies. Module/manifests still contain comments saying it is off by default; executable feature declarations take precedence.
- Condition/classification: Linux backend embedded in app; gated by feature, not target. No assertion that other OSes cannot compile GStreamer, but `/dev/video*` and v4l2src are explicitly Linux.
- Impact: discovery, permission mediation, device identity and frame acquisition cannot vary by host; public resources currently receive device-path strings (see `apps/camera/src/actions.rs:1146`, `apps/camera/src/search.rs:288` test fixtures).
- Boundary/migration: introduce semantic camera enumeration, opaque device IDs, permission-aware capture sessions and bounded frames; move V4L2/GStreamer lifetime and callbacks behind Linux platform backend. Preserve latest-frame queue, frame-size bounds and honest unavailable results. Keep camera modes, saved captures and resource actions app-owned; map opaque IDs to resources instead of assuming paths.
- Acceptance: production discovery/open tests through adapter, deterministic unavailable/denied/device-removed cases, frame bounds/backpressure, backend teardown, and an explicitly enabled Linux real-device job requiring actual frames. Non-Linux adapter must report support or unavailability without probing `/dev`.

### A02-02 — Camera launches xdg-open directly (high)

- Evidence: `apps/camera/src/app.rs:2541` spawns `xdg-open`; `:2233` uses it for `settings://privacy/camera`; `:2364` for scanned URI. `apps/camera/tilo-app.toml:51` declares host URI mediation (`open-uri = "via-host"` nearby).
- Condition/classification: leaked Linux desktop command; not target-guarded. Spawn success is treated as handoff success, without observing host acceptance.
- Boundary/migration: route both destinations through semantic host `open_uri` / open settings capability, preserving URI validation and consent in the existing host portal path. Do not add unrestricted exec/network permissions to make sandbox launches work.
- Acceptance: fake host records exact URI; denial/unavailable/error reach app state; integration test verifies real host invocation rather than subprocess launch. Inspect shared portal public API before adding a duplicate.

### A02-03 — Maps embeds the XDG location implementation (high)

- Evidence: `apps/maps/src/location.rs:53` worker; `:91` ashpd LocationProxy/session/Exact accuracy; `:118` default WindowIdentifier; `:120` timeout and update; `:143` portal error conversion. `apps/maps/Cargo.toml:36` unconditional ashpd dependency.
- Condition/classification: intentional Linux portal backend in app, not generic map logic. Permission prompt is deliberately the security boundary.
- Boundary/migration: platform location request with accuracy, parent-window context, timeout, cancellation and typed denied/unavailable outcomes. Move ashpd, session cleanup and private runtime to Linux backend; retain routing/UI conversion in Maps. Reuse current LocationOutcome shape where possible.
- Acceptance: denied, missing service, timeout, cancellation and success exercise real app drain path; backend test closes session on every exit. No silent direct GeoClue/GPS bypass.

### A02-04 — Music network policy probes NetworkManager itself (medium)

- Evidence: `apps/music/src/net.rs:14` lifetime OnceLock and unknown→false; `:36` Linux-only `nmcli -t -f GENERAL.METERED device show` through bounded-command; `:68` other targets return unknown. `apps/music/Cargo.toml:73` target-scoped dependency.
- Impact/classification: explicit platform branch correctly guarded but misplaced; stale lifetime cache and unknown-as-unmetered are app policy, not necessarily platform facts.
- Boundary/migration: platform connection-cost state (metered/unmetered/unknown, preferably updates); preserve explicit app prefetch policy separately. Move nmcli or equivalent native query to Linux backend and keep bounded execution.
- Acceptance: target-independent policy tests cover unknown and changes; Linux parser/process tests cover missing command, timeout and guessed states; app code has no NM command/target cfg.

### A02-05 — Music duplicates native playback and media-session backends (high)

- Evidence: `apps/music/src/audio.rs:187` stores GStreamer elements; `:273` initialization/playbin; `:1149` explicit audio sink bin; `apps/music/src/mpris.rs:264` feature-gated D-Bus server; `:588` session bus registration. `apps/music/Cargo.toml:23` couples audio, zbus, Tokio and file-chooser features; defaults enable audio/live.
- Condition/classification: GStreamer is cross-platform native middleware, not intrinsically Linux; sink/plugin setup belongs behind media platform boundary. MPRIS is Linux/freedesktop integration. Pure state/command cores and music streaming selection remain app policy.
- Boundary/migration: playback session with load/play/pause/seek/position/output health; now-playing session with metadata and semantic command stream. Reconcile with evidenced `media::MediaEngine` dependency and Tube's `ui/mpris` integration rather than build third equivalents. Move GStreamer element introspection, bus service and dependencies to backend packages, preserving bounded staging and stop/drop behavior.
- Acceptance: existing playback semantics and MPRIS command/metadata tests run against adapter; Linux live output test proves real sink activity; no fake sink counted as audible success. Cross-host backend selection does not require app-owned zbus.

### A02-06 — Editor owns native decode, PCM output and export plumbing (high)

- Evidence: `apps/video-editor/src/audio.rs:585` uridecodebin→PCM appsink; `:1198` live GStreamer output pipeline; `:1229` autoaudiosink. `apps/video-editor/src/waveform.rs:91` separate PCM decode pipeline. `apps/video-editor/src/export.rs:757` native runner, `:790` init, `:812` launch, buffers/EOS/cancellation through `:994`. `apps/video-editor/Cargo.toml:57` defaults and feature definitions (`decode`/`gst`) carry native dependencies. Export descriptions and codec policy begin around `export.rs:402`.
- Condition/classification: deliberate native media backends mixed with editor domain logic. GStreamer APIs are portable middleware but native loading/plugin capabilities must be platform-owned.
- Boundary/migration: consolidate PCM decode and output streams plus encode/mux jobs beneath shared `crates/media/core` platform integration. Keep timeline mixing, transforms, codec *preferences*, progress presentation and project data in editor. Expose supported codecs/output capabilities; preserve hardware/software fallback logic instead of leaking plugin names into UI contracts. Preview/thumbnail already use shared MediaEngine (`preview.rs:240`, `thumbnail.rs:838`) and provide the precedent.
- Acceptance: generated clips exercise PCM decode/waveforms, seek, export video+audio fidelity, cancellation and failure on unavailable codec. Existing export fidelity tests must retain assertions, not switch to stand-in images. Native backend dependency can be removed from app runtime (backend integration tests may still need it).

### A02-07 — Tube private persistence contains Unix security primitives (high)

- Evidence: `apps/tube/src/history_store.rs:9` unconditional Unix MetadataExt/OpenOptionsExt/PermissionsExt; `:248` 0700 parent; `:257` atomic-file 0600 write; `:405` geteuid; `:413` bounded no-follow read; `:432` O_NOFOLLOW and `:437` uid/dev/ino validation. `apps/tube/src/sync/lifecycle.rs:12` Unix imports; `:145` peer persistence applies 0700/0600, exclusive temp create, fsync/rename/directory fsync. Tube Cargo `:44` libc dependency is unconditional.
- Condition/classification: intentional security backend embedded in app, Unix-only API use (including macOS, not only Linux). Privacy guarantees are requirements, not incidental code to delete.
- Boundary/migration: private persistent-file API with bounded read, owner validation, no-follow identity checks, owner-only storage and durable atomic replacement. Extend/reuse `atomic-file` for write aspects; do not assume it currently covers secure read. Keep history merge and Grayjay serialization in app. Native ACL/identity backend must implement equivalent semantics on other targets or fail closed.
- Acceptance: preserve symlink refusal, unexpected owner, races/replacement, oversized file, permissions tightening and durable replacement tests (`history_store.rs:463` onward); equivalent non-Unix security tests. Never replace this with plain read/write solely to compile.

### A02-08 — Tube explicitly binds account/device secret acquisition to XDG (high)

- Evidence: `apps/tube/src/sync/lifecycle.rs:408` production_parts calls `runtime_portal_xdg::Secret::retrieve`, rejects short key material, derives domain-separated device key; `apps/tube/Cargo.toml:57` dependency and `:87` feature membership. Default `grayjay-sync` enables it.
- Classification/impact: Linux implementation crate is directly selected by app; host secret mediation is already correctly used but target selection should not live here.
- Boundary/migration: host-secret/key-material capability with stable per-app scope and typed denied/unavailable outcome, backed by existing XDG service on Linux. Keep Zeroizing, domain separation and peer protocol app-owned. Do not replace with plaintext keys or generate throwaway identity on unavailable broker.
- Acceptance: restart yields same identity for same scope, different scopes isolated; denied/short/missing material fails closed; no secret in CRDT/history/logs. Verify XDG and alternate host implement same lifecycle contract.

### A02-09 — Multiple apps duplicate platform directory selection (medium)

- Evidence/files: `apps/music/src/storage.rs:400` XDG_DATA_HOME/HOME/.local/share/current-directory fallback; `apps/music/src/backend/live.rs:152` XDG_CACHE_HOME/HOME/.cache/temp; `apps/maps/src/tiles.rs:948` equivalent cache selection; `apps/photos/src/app.rs:493` XDG_STATE_HOME/HOME/.local/state; `apps/video-editor/src/project_file.rs:294` expressly duplicates per-package state selection; `apps/tube/src/sync/lifecycle.rs:425` XDG_STATE_HOME then dirs::state_dir. Related centralized-library consumers: `apps/camera/src/library.rs:25` picture_dir/home fallback; `apps/tube/src/history_store.rs:121` data_local_dir; `apps/tube/src/app.rs:64` data_local_dir.
- Classification: manual XDG/default policy is extraction; dirs crate calls already abstract OS discovery but per-app fallback/storage policy should use same capability. Generic temp_dir is not itself Linux-specific.
- Boundary/migration: semantic app data/state/cache, user pictures, and approved document destination locations. Preserve established directory names, package-ID validation, env overrides used in tests and migrate existing contents; avoid silently moving private state into cwd. Do not conflate collaborative document storage with machine-local cache/profile.
- Acceptance: stable existing Linux paths and upgrade read/migration, injected roots for tests, missing home/unwritable directory explicit handling, native target locations, cache eviction independent of state. Comments in maps still mention /var/cache although implementation does not use it—do not report that nonexistent runtime path as a defect.

### A02-10 — Editor resolver hardcodes FUSE mount and equates stat with hydration (high)

- Evidence: `apps/video-editor/src/media.rs:36` WORKSPACE_ROOT=/var/workspace; `:145` resolve tests exists() and labels Local; `:172` mount resolution; `:201` engine_source also consults shared `media::Source::workspace_root`; `:214` stored_path_for_import checks both roots. Missing reference falls back to fixed mount. This conflicts semantically with root instruction that metadata can exist before bytes hydrate; source inspection does not prove an observed runtime failure.
- Boundary/migration: platform/workspace media resolution using logical resource reference, mount access and content availability separately. Reuse existing Source::workspace_root for root discovery where applicable; app should not retain a second default independently. Keep project-relative and explicit imported paths as distinct reference kinds and preserve hydration on first read.
- Acceptance: custom workspace root resolves, projected metadata with absent local content is not called hydrated, first read can hydrate, peer references preserve identity, project-local precedence and unavailable content tests retain existing intentions.

### A02-11 — Music hand-builds file URIs (medium)

- Evidence: `apps/music/src/app.rs:2783` cached-track playback URI; `apps/music/src/audio.rs:565` staged stream URI. Test fixtures repeat at `audio.rs:1336` and `:1418`.
- Classification/impact: leaked path representation assumption; even Unix filenames with #/%/spaces need URI encoding, and other host path syntax differs. No claim of reproduced playback failure.
- Boundary/migration: reuse a verified local-path→URI helper (`media::Source::to_uri` is used by editor waveform; Music already depends on url and media). Preserve machine-local URI privacy boundary.
- Acceptance: round trips for spaces, Unicode, #, %, platform roots; remote authorities rejected when mapping back to local paths. Cover resource/actions still exclude local file URIs.

### A02-12 — yt-dlp availability/execution is a host integration requirement (medium)

- Evidence: `apps/tube/src/backend/ytdlp.rs:58` fixed executable name and version probe; `:77` all invocations use bounded-command with output/time limits. `apps/tube/Cargo.toml:77` default live enabled (despite stale comments).
- Classification: external-tool backend, not intrinsically Linux-only; already uses shared process executor. YouTube query mapping belongs in Tube; executable discovery, sandbox execution and lifecycle belong in platform tooling capability.
- Migration/acceptance: use managed host tool resolution while preserving bounded-command semantics; test missing tool, denied launch, timeout, bounded output and normal parsing. Keep backend fallback visible; no unbounded process or shell-string replacement. Distribution must explicitly supply/mediate the tool; do not broaden sandbox blindly.

## Intentional backends, tests and deployment records

### A02-13 — Native-media test environment assumptions must remain explicit

- Linux assumptions: `apps/music/src/audio.rs:1290` and `apps/music/src/app.rs:7399` discover PipeWire/Pulse via XDG_RUNTIME_DIR sockets/PULSE_SERVER. `apps/music/src/mpris.rs:693` uses Tokio UnixStream pair for MPRIS roundtrip. `apps/camera/src/capture.rs:248` opt-in live device test, `:255` skips no device and `:264` logs open failure. `apps/video-player/tests/real_decode.rs:44` invokes gst-launch-1.0 and `:99`, `:109`, `:182`, `:187` return on unavailable prerequisites. Music real-output tests also return on absent plugins/server (`audio.rs:1323`).
- Classification: test-only native backend requirements, not application runtime leaks. Existing success-on-unobserved-subject paths conflict with verification covenant when offered as real-device/decode proof.
- Migration: backend-specific integration suites with provisioned declared prerequisites and hard failures when explicitly selected; pure app tests use injected semantic adapters. Keep Unix D-Bus socket testing in Linux backend suite. Do not use global target gating as a substitute for testing backend guarantees.
- Acceptance: selected hardware/decode suite fails if unavailable or no frame/audio observed; fake tests cannot claim real media proof; portable tests do not detect availability solely through Linux socket names.
- Other native integration sources to carry forward: `apps/video-editor/tests/support/generated_clip.rs:7`, `apps/video-editor/tests/export_fidelity.rs:47`, `apps/video-editor/src/preview.rs:1141`, `apps/video-editor/src/export.rs:1197`. Editor export tests already demand their prerequisites rather than skip. Examples `apps/video-editor/examples/{compose_probe,frame_cost,export_probe}.rs` are diagnostic GPU/export tools, not deployment runtime. XDG-isolation tests in `apps/video-editor/tests/headless_action_no_window.rs:33`, `apps/video-editor/src/project_file.rs:662`, `apps/photos/tests/framework_history_launch.rs:48`, `apps/weather/src/framework_history_tests.rs:451` should inject new path contracts instead of assuming XDG works on every host.

### A02-14 — App manifests intentionally describe Linux deployment

- Files: `apps/camera/tilo-app.toml`, `apps/image-editor/tilo-app.toml`, `apps/maps/tilo-app.toml`, `apps/music/tilo-app.toml`, `apps/photos/tilo-app.toml`, `apps/tube/tilo-app.toml`, `apps/video-editor/tilo-app.toml`, `apps/video-player/tilo-app.toml`, `apps/weather/tilo-app.toml`.
- Evidence: each contains package ISA and sandbox namespaces/seccomp/landlock; examples camera `:22` ISA, `:34` sandbox; Tube `:34`; video editor `:34`. Some comments describe /usr/lib64, /var/workspace, Pictures/Videos, or feature defaults.
- Classification: retain target-specific packaging, not application logic to move into a generic runtime API. Kernel enforcement must remain strict. Coordinate capability declarations with backend relocation, especially camera URI/capture, Maps location and Tube secrets. Native GStreamer dependencies are feature-gated but commonly default-enabled; comments are not reliable build-matrix evidence.
- Acceptance: package manifests match actual feature/runtime capability requirements, Linux sandbox tests still enforce permissions, alternate target packaging generated separately. No weakening of Landlock/namespaces for portability.

## Reviewed false positives / existing seams to retain

- `apps/video-editor/src/gpu/{mod,compositor,media_source,title}.rs` and `composite.wgsl` use portable wgpu/Vello computation. `gpu/mod.rs:159` explicitly obtains shared GPU from framework; no app-owned Metal/Vulkan platform selection found. Keep timeline composition/effects app-owned; hardware adapter creation remains existing shared boundary. GPU requirement is not by itself OS-specific source.
- `apps/video-editor/src/portal.rs:21` imports shared ui::portal types and reexports outcomes; it carries app filters and workflow, not ashpd implementation. Image editor uses shared portal; video player selects ui/portal through Cargo. Do not extract these wrappers simply because they say XDG in comments.
- `apps/weather/src/region.rs:49` already calls settings-store. `apps/video-player/src/preferences.rs` likewise consumes settings-store. Shared implementation is adjacent scope, not duplicated here. Weather/maps HTTP, chrono time handling, pure image/EXIF/PNG code, generic files/TCP/threads are not automatically platform-specific.
- Camera scanner paths in `apps/camera/src/app.rs` already consume `os_actions::printing` types/services; backend extraction belongs to that service audit. Camera's `video.rs` encodes container bytes and uses generic fs; no native video API found there.
- `/var/...` fixtures in Photos (`model.rs:746`, `reference.rs:79`, `doc.rs:1419`, `actions.rs:2787`, `tests/end_to_end.rs:593`) and `/tmp` fixture expectations in video-player and editor are chiefly identity/sample strings, not proof that runtime uses those directories. Portable filename/path fixtures can be reviewed with directory migration without mass replacement.
- All app main/thin runtime entry points and provider manifests were inventoried; no scoped build.rs or shell scripts surfaced. No vendor subtree in assigned app scope; Rustypipe/rquickjs dependency internals were not inspected.

## Handoff

14 stable grouped records: 12 extraction/adapter candidates, one test/backend verification record, one intentional packaging record. Adjacent scopes requiring coordination: `crates/media/core`, `crates/system/{atomic-file,bounded-command,os-actions,settings-store}`, `crates/runtime/runtime-portal-xdg`, workspace fs service and OS image tooling. Existing `ui`/`crates/app` public APIs are reuse evidence only; their internals remain excluded. Most urgent: preserve Tube private-file/secret guarantees, centralize editor workspace/hydration resolution, move V4L2/location/MPRIS integration behind semantic platform contracts.
