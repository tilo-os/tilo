# A12 — Agent, developer, document, format, and Windows compatibility crates

## Scope and method

Scope: source, manifests, tests, build scripts, and documentation under `crates/{agent,dev,doc,format,windows}/`. Application-framework internals are outside this audit.

Destination alignment: `docs/architecture/platform-services.md` proposes `ui::platform` as facade and focused `crates/system`/`crates/runtime` implementation crates. Every proposed capability below belongs in those focused non-rendering service crates (or an existing owning service), optionally re-exported through that facade. Headless binaries must depend directly on service crates, not the renderer. The spec is proposed, not evidence that the named adapters already exist.

The audit covers target guards, OS modules, libc, paths, sockets, credentials,
permissions, process lifetime, GPU and runtime dependencies, and surrounding
production and test code. External dependency internals and adjacent launcher
or system services are covered separately. Findings describe source coupling
and inferred portability impact rather than observed runtime failures.

## Actionable findings

**Current-tree note:** A12-10's `crates/dev/tilo-dev/...` and
`crates/dev/tilo-dev-template-smoke/...` locations are historical source
anchors. The current tree contains `crates/dev/tilo-app-cli` and
`crates/dev/tilo-app-template-smoke`, but no one-to-one replacement was
verified for the cited implementation. Retain the developer-tooling finding
without presenting those old paths as current files.

**Source-anchor note:** Verified prefix repairs identify current moved paths where
available. Any `:line` suffix remains the original 2026-09-18 audit coordinate; it
is historical evidence and does not assert that the cited symbol is at that line
in the current tree.

### A12-01 — Model gateway transport exposes Unix descriptors and Linux peer authentication

**Classification:** extract leaked transport ABI; retain authenticated Linux backend. **Priority:** high. **Condition:** Unix client types; Linux daemon credentials.

**Evidence:** `crates/agent/model-gateway/src/lib.rs:8` imports fd/UnixStream unconditionally; `:117` and `:132` consume an environment-provided inherited descriptor, validate with `fcntl`, duplicate with `F_DUPFD_CLOEXEC`; `:279` and `:305` tie otherwise generic framing to UnixStream. `crates/agent/model-gateway/src/main.rs:108` binds/chmods a local socket; `:125` owns systemd LISTEN_PID/LISTEN_FDS/fd-3 activation; `:753` checks Linux `ucred`/SO_PEERCRED/geteuid. `crates/agent/model-gateway/Cargo.toml:26` has unconditional libc.

**Impact:** client protocol and gateway orchestration cannot be separated cleanly from POSIX handle transfer; daemon identity checks specifically require Linux. Authentication must survive migration.

**Boundary/migration:** introduce an authenticated inherited service-channel capability with owned channel handles and typed peer identity; keep systemd activation, credential probing and CLOEXEC duplication in platform backend. Generalize frame serialization over Read/Write, preserving bounded frames. Do not replace inherited capabilities with ambient reconnect-by-path access. Compose daemon with backend-selected listener.

**Acceptance:** gateway framing/protocol tests operate on in-memory streams; Linux tests verify malformed handoff, wrong peer, invalid fd and activation rejection, ownership/closure and frame limits; non-Linux supported target can compile protocol/client facade without Linux ucred. Existing `tests/security_boundary.rs:3` checks source text: update its moved-file location but supplement with production-boundary assertions rather than relying on that lexical check alone.

### A12-02 — Local model provisioning already has a boundary, but daemon mixes hardware, Bootc and service management

**Classification:** intentional Linux/Bootc backend embedded in service entry point; extract. **Priority:** high.

**Evidence:** `crates/agent/local-model-service/src/main.rs:242` hardcodes system-extension socket/status; `:392` defines SystemdSupervisor; `:393` builds Podman vLLM argv with GPU devices, host networking, read-only model mounts and SELinux treatment; `:443`, `:453`, `:482`, `:524`, `:532` execute Podman/systemd tooling. `:546` probes `/proc/meminfo`, `:560` measures `/var/lib/tilo/models`, `:565` calls nvidia-smi, `:586` reads ROCm version, `:590` reads sysfs DRM; `:630` uses statvfs. `:1155` selects privileged/user paths, `:1174` implements XDG/HOME model storage; `:1196` connects extension API; `:1454` and `:1492` enqueue durable systemd workers. `:875`, `:983`, `:1100`, `:1117` repeat inherited fd, Linux peer credentials, mode-0600 sockets and systemd activation. Manifest `crates/agent/local-model-service/Cargo.toml:30` declares libc.

**Impact:** platform decisions are coupled to user-visible provisioning/state orchestration. In particular hardware free-space probe uses a fixed system directory while actual model root is per-user/override; the adapter should inspect the selected store volume, not assume those mounts coincide.

**Existing contracts to reuse:** `crates/agent/local-model-service/src/lib.rs:43` HttpsTransport and `:57` ProcessSupervisor (install/start/stop), and `crates/agent/provider-config/src/local.rs` LocalModelBackend/HardwareSnapshot/RuntimeKind. Provider-config policy and validation are platform-independent and should stay outside platform. CUDA/ROCm runtime enum values are supported product capabilities, not themselves an OS syscall leak.

**Boundary/migration:** move SystemdSupervisor into Linux backend implementing existing ProcessSupervisor; add trusted hardware snapshot, durable model job scheduler, signed extension activation/status and trusted model location capabilities. Reuse A12-01 authenticated-channel primitive. Preserve signed digest/loopback constraints and privileged override rejection. Hardware probe receives the actual storage root. An unsupported runtime is explicitly unavailable, not silently launched insecurely.

**Acceptance:** fake hardware/backend tests cover CPU, missing driver, accelerator setup, errors and state progression; Linux integration verifies rootless Podman/systemd lifecycle, download continuation and cancellation, extension status, private endpoints, signed-image check, and free-space selection on a different mount. Portable policy compiles without systemd/libc imports.

### A12-03 — Private model files and release signer need semantic secure-file operations

**Classification:** platform filesystem/security backend; guarded non-Unix behavior differs. **Priority:** high for preserving security invariants.

**Evidence:** `crates/agent/local-model-service/src/lib.rs:812` chmods directories 0700, `:826` files 0400; `:821` and `:832` non-Unix functions return success without enforcing privacy. `crates/agent/local-model-service/src/bin/tilo-model-catalog.rs:58` implements Linux openat2 with NO_SYMLINKS/NO_MAGICLINKS; `:104` non-Linux fallback uses O_NOFOLLOW only on Unix and ordinary OpenOptions elsewhere. `:575` disables Linux signing-process core dumps with PR_SET_DUMPABLE; `:602`/`:606` check Unix signing-key mode and ownership and `:614` checks single hardlink count. `:253`/`:255` use existing atomic_file for output mode 0644. `RELEASE.md:18` documents release-host secret/output paths.

**Impact:** security operations are spread between runtime and release tool; fallback branches do not promise equivalent ancestor-link/private-file checks. Do not assume these source differences establish an exploitable runtime path on an unsupported OS.

**Boundary/migration:** reuse/enhance atomic_file and shared secure filesystem layer with `open_regular_no_links`, `verify_private_owned_secret`, private immutable artifact promotion and atomic publication. Linux openat2 implementation remains backend code. Non-Unix implementation must provide equivalent ACL/reparse-point semantics or explicitly reject unsupported secure operation. Release CLI can remain deployment tooling and call that API.

**Acceptance:** production-path tests reject final and intermediate links, FIFO/nonregular input, permissive/wrong-owner/multiply-linked signing key, enforce required dump protection, and preserve existing signing/catalog bytes. Unsupported backend fails closed; private-artifact permissions remain verified after extraction/promotion.

### A12-04 — Format host's public transport module and configuration fallback leak local OS choices

**Classification:** extract transport/locations; retain runtime/build tooling. **Priority:** medium.

**Evidence:** `crates/doc/format-host/src/lib.rs:35` unconditionally exports transport and `:46` falls back from dirs::config_dir to literal `/tmp`; `src/transport.rs:8` uses Tokio UnixListener, `:15` resolves XDG_RUNTIME_DIR/temp, `:30` binds/removes socket and `:40` ignores chmod failure; `:74` handle_connection requires UnixStream. `src/main.rs:35` selects paths and `:66` binds; `tests/integration.rs:87`/`:99` depend on Tokio UnixStream. `src/runner.rs:220` already uses cross-platform dirs::cache_dir. `src/watch.rs:36` uses notify::recommended_watcher despite Linux-specific 'inotify' comments. `Cargo.toml:18` uses Wasmtime/Cranelift; `build.rs:6` requires external capnp codegen.

**Impact:** portable router/registry/component logic inherits a Unix transport dependency. Temp fallback and ignored permission failure are material when defining a private service endpoint; do not reproduce best-effort privacy in the abstraction.

**Boundary/migration:** platform supplies private authenticated local endpoint and config/cache locations; adapt Cap'n Proto network over generic async read/write. Keep Wasmtime execution limits/epoch policy in format service; its JIT target support is a runtime dependency contract rather than bespoke platform code to rewrite. Keep capnp build dependency as documented build tooling. Reuse notify rather than replacing its backends.

**Acceptance:** same router tests over generic channel, Unix endpoint permissions/creation failures enforced, predictable no-config-root error or secure per-user fallback, real watcher reload integration on supported hosts, and external capnp prerequisite documented. Update `format-router.capnp:5` and `README.md:25` endpoint documentation.

### A12-05 — Document blobstore chooses Linux-style cache root itself

**Classification:** leaked location policy; small extraction. **Priority:** medium.

**Evidence:** `crates/doc/qdoc/src/blobstore.rs:277` default_root resolves TILO_BLOB_STORE, XDG_CACHE_HOME, HOME/.cache, then temp_dir. `:265` uses std File locking (already portable API); `:635` explains sandbox-local blobs.

**Impact:** host cache placement is inconsistent with native non-Linux directory conventions and fallback may lose intended per-user/session scope. Storage data model, hashing, file-lock algorithm and blob protocol are not OS-specific.

**Boundary/migration:** inject platform/sandbox-provided blob cache root; preserve explicit TILO_BLOB_STORE as controlled developer override and existing constructor with explicit root. Do not centralize into one shared host path that breaks per-workspace isolation. Reuse shared directory capability used by A12-02/04.

**Acceptance:** location tests cover explicit root, isolated sandbox roots, native host cache and unavailable-directory handling; existing blobstore durability/concurrency/transfer tests remain unchanged in substance.

### A12-06 — Windows compatibility planning exposes the Linux capsule representation

**Classification:** deliberate Linux Wine/UMU/FEX backend, mixed with platform-independent compatibility policy. **Priority:** high boundary design.

**Evidence:** `crates/windows/compat-service/src/lib.rs:30` includes all backend modules without target selection; `Cargo.toml:16` launcher and `:17` libc and `:21` windows-vm are unconditional. `src/plan.rs:15` onward fixes capsule/runner/layout paths; `:122` RuntimeRegistry and `:390` LaunchResourceBroker are existing seams; `:259` HostSession includes Unix user/runtime assumptions; `:396` CapsuleCommandPlan contains Bubblewrap representation and `:422` raw directory fds; `:427` SandboxPlanner assembles mount/env/Wayland/audio/network arguments. `src/execute.rs:26` SandboxExecutor exists, while `:35` BwrapExecutor obtains uid/gid, `:84` runs `/usr/bin/bwrap`, `:101` maps child fds and `:155` creates Linux memfd identity files. `src/service.rs:330` VmDelegate and `:600` generic CompatibilityService already separate some policy. `src/catalogue.rs:23` chooses shipped catalogue path.

**Impact:** even consumers of explain/catalogue/service policy inherit a Linux process/mount ABI. This is Windows *guest* support on Linux, not a Windows host backend; preserve that distinction.

**Boundary/migration:** keep compat-catalogue, installer format inspection, protocol/route decisions and canonical capability resolution domain-side. Move SystemRuntimeRegistry, SandboxPlanner/CapsuleCommandPlan and BwrapExecutor into Linux compatibility backend as a coherent unit, or hide capsule plan as backend-private associated type. Reuse existing registry/executor/delegate/resource seams; a new generic 'run command' interface would lose security meaning. Backend advertises supported routes, keeps fixed trusted runners and allowlists, accepts canonical capability plan and owned resource leases. Catalogue location supplied by trusted deployment directory API.

**Acceptance:** route/explain/installer policy builds independently of Linux backend; Linux plan tests retain mount, identity, environment, network and deny-by-default assertions; live bwrap/Landlock integration remains required on designated Linux job. Unsupported hosts explicitly report unavailable routes.

### A12-07 — Wine prefix store/DXVK deployment contain deliberate secure Unix filesystem implementations

**Classification:** retain within Linux compatibility backend; share secure file primitives where compatible. **Priority:** high.

**Evidence:** `crates/windows/compat-service/src/store.rs:5`/`:6` imports fd and Unix OpenOptionsExt; `:573` O_NOFOLLOW, `:635` onward controls DOS drive symlinks, `:699` exposes directory fds, `:715` onward implements open/openat/mkdirat/renameat/unlinkat. `src/dxvk.rs:11`/`:12` uses Unix metadata/fds, `:18` fixes prefix/payload/build metadata, `:85`/`:140` flock lease, `:292` O_PATH, `:400` onward fd-relative nofollow IO and `:481` atomic rename.

**Impact:** persistent domain records are coupled to Linux prefix handles. Mechanical substitution with path-based std IO could regress race/link protection and synchronization.

**Boundary/migration:** move prefix materialization/DOS links/DXVK install and lease ownership together behind capability-preserving `ManagedPrefix`/artifact-deployment backend. Keep serialized capsule records and validation portable where practical. Reuse shared nofollow atomic-file primitives only when they preserve directory-handle anchoring, hardlink handling and lock lifetime; do not widen grants through host drive links.

**Acceptance:** existing store symlink and DXVK symlink/hardlink/FIFO tests exercise production backend; concurrent preparation preserves locking and rollback, revoked handles cannot reopen host paths. Keep fd ownership opaque outside backend.

### A12-08 — Compatibility session authentication and fd-passing are Linux IPC

**Classification:** platform identity/transport adapter; retain Linux implementation. **Priority:** high.

**Evidence:** `crates/windows/compat-service/src/session.rs:4`–`:6` import Unix fds/sockets; `:368` determines uid, `:377` authenticates launcher via executable `/proc/{pid}/exe`, `:421` reads SO_PEERCRED, `:450`/`:459` parse proc status/stat (parent/start identity); `:505`/`:510` bind/recover session socket; `:1281` SessionClient owns UnixStream; `:1351`/`:1428` transfer SCM_RIGHTS with recvmsg/sendmsg, CLOEXEC and ancillary validation.

**Impact:** protocol and session authority cannot be hosted independently of Linux authenticated process identity and descriptor transfer. Numeric uid/pid alone are insufficient replacements for the existing launcher checks.

**Boundary/migration:** factor authenticated launcher identity, channel and transferable resource-handle envelope into platform contracts; preserve process incarnation/start-time binding, trusted launcher registry and capability authority. Reuse A12-01 channel primitive if it can express stronger identity/handle transfer; do not collapse checks to same-user only. Keep protocol structs independent.

**Acceptance:** adversarial production tests cover wrong executable/uid/process ancestry or incarnation, malformed/truncated/multiple ancillary handles, cleanup/CLOEXEC, oversized frames and invalid handoff. Keep native Linux session integration; portable protocol tests need no procfs.

### A12-09 — Compatibility audio, VM presentation and translated process lifecycle are native backend implementations

**Classification:** deliberate platform backends; retain behind platform boundary. **Priority:** high.

**Evidence:** `crates/windows/compat-service/src/resources.rs:14` fixes lease/network roots, `:36` gets uid, `:76` materializes audio/egress grants; `src/audio_proxy.rs:19` uses Unix metadata/modes, `:27` fixes PipeWire programs/share config, `:111`/`:122` launches private graph, `:140` fixes Linux environment, `:157` selects Pulse user socket, `:230` verifies directories and `:261` sockets. `src/vm_adapter.rs:439` describes rootless Xwayland RAIL; `:453`/`:515` gets uid, `:456`/`:518` fixes runtime root and `:489` helper executable. `src/translated.rs:20` runs FEX/Wine lifecycle, `:196` acquires SIGCHLD/subreaper guard, `:310` inspects proc children, `:345`/`:355` uses pidfd syscalls, `:421` runs FEX with process group, `:448` fixes runtime environment, `:465` uses pipe2/poll readiness, `:536` kills/reaps managed groups. `src/bin/tilo-windows-runtime.rs:3` calls that backend.

**Impact:** audio privacy and process cleanup depend on platform semantics; these should not be forced into generic portable implementations. VM domain orchestration and portal/collaborator grants can remain portable, while launchers are native.

**Boundary/migration:** move SystemResourceBroker/AudioProxyLease and concrete RAIL/SPICE launchers to Linux backend, reusing LaunchResourceBroker/VmDelegate and adjacent windows-vm interfaces. Expose revocable audio/presentation/egress leases, preserving no microphone by default. Keep FEX descendant management and prefix preparation together behind supervised translated-runtime operation; explicit unsupported capability on hosts lacking guarantees.

**Acceptance:** private graph never grants raw host audio or /dev/snd, microphone requires canonical grant and revocation terminates graph; VM presentation preserves grants. Existing detached-descendant tests must prove cleanup does not kill prior unrelated children, subreaper/SIGCHLD state restores, readiness timeout cleans up, and pid reuse cannot misdirect signals.

### A12-10 — Developer tooling can stay host-specific, but its reusable library/build consumers inherit it

**Classification:** intentional host tooling with optional extraction of shared primitives; low/medium priority.

**Evidence:** `crates/dev/tilo-dev/src/lib.rs:11`/`:12` unconditionally imports Unix permission/process extensions; `:3891` defaults mode 0644; `:4039` invokes capnp; `:4576` supplies XDG test directories; `:5358`/`:5379` creates process groups; `:5434`/`:5447` terminates groups; `:5451`–`:5498` installs/restores POSIX signal handlers. Manifest `crates/dev/tilo-dev/Cargo.toml:18` libc and `:19` notify. `crates/dev/tilo-dev/src/main.rs:1142` invokes Cargo; subsequent run/audit paths run built executable. `crates/dev/tilo-dev-template-smoke/build.rs:7` invokes tilo-dev render_component_source, so host build-script dependency inherits whole library OS coupling. `crates/dev/ui-builder/src/main.rs:46` branches Unix permission preservation around existing atomic_file. `crates/dev/collab-link/src/main.rs:50`/`:131` is explicitly Unix socket fault proxy. `crates/dev/collab-probe/src/main.rs:82` reports inherited collab fd (actual transport lives in adjacent crate). `crates/dev/frame-link/src/bin/flatpak-share-demo.rs:299`/`:653` hardcodes Fedora/Linux font candidate paths and `:840` invokes ffmpeg.

**Migration:** developer commands, Linux fault proxy and Flatpak demo may remain declared host-specific tools. If cross-host generation is needed, split pure template/scaffold/UI-graph generation from Unix dev supervision so smoke build and consumers need not import it. Reuse platform supervised-process lifecycle and atomic-write-with-preserved-permissions where already available; keep notify's portable watcher. Demo can accept font/encoder assets or use platform font lookup if cross-host rendering is desired; ffmpeg itself is an external dependency, not proof of Linux-only code.

**Acceptance:** document supported tool hosts; pure generation tests compile on intended build hosts; Linux dev stop/restart still cleans descendants and restores signal handlers; UI graph save preserves permissions and atomicity; demo reports unavailable assets/tools explicitly.

## Intentional targets, tests, and negative findings

- `crates/doc/qdoc-codec-markdown-wasm/src/lib.rs:31` intentionally gates component export to wasm32; host no-op described at `:14`. Keep guest target boundary and manifest component setup; it is not leaked host OS logic.
- `crates/doc/{qdoc-epub,qdoc-html,qdoc-rtf,qdoc-pdf}` parsers/renderers and remaining qdoc model/codec files showed no platform FFI/native runtime dependence in the scan. qdoc-pdf WinAnsi and Helvetica metrics are PDF format concepts, not Windows APIs. qdoc-pdf example `/tmp` is usage documentation. Existing qdoc blobstore subprocess test (`src/blobstore.rs:592`) invokes current_exe and is portable at the API level.
- `crates/windows/compat-catalogue/src/lib.rs:1622` `/tmp/program/app.exe` and `:1947` `/tmp/evil.exe` are data/validation test fixtures, not executable host path defaults. Installer PE parsing, compatibility catalogue formats and guest Windows paths should stay domain-side.
- `crates/doc/format-provider-wit` WIT/generated bindings are protocol; no bespoke OS implementation found. `format-host/src/watch.rs`, `runner.rs`, `router.rs` comments mention inotify/cache, but actual notify/dirs APIs already abstract those host details.
- `crates/dev/{headless-schema,ui-graph,frame-link}/src/lib.rs` are data/model/simulation logic. `tilo-dev/templates/data.rs` scan hit is a generated identifier containing 'from_model', not chmod/OS logic. Template source references to UI remain out of app-framework extraction scope.
- `crates/agent/provider-config` serializable provider/runtime policy is portable; retain it outside platform. Hardware categories can be expanded with future backend support rather than moving the whole policy crate.
- **Backend test placement:** model-manager `main.rs:1534` UnixStream pair; compatibility `catalogue.rs:188` Unix symlink, `store.rs:1050` symlink protection, `dxvk.rs:534` symlink/FIFO/locks, `session.rs:1633` fd/socket and `:1742` proc identity, `translated.rs:585` Linux process cleanup, `plan.rs:1364` bwrap/Linux integration and `:1478`–`:1485` Unix/Landlock prerequisites, `audio_proxy.rs:341` live Linux graph. Move with native implementation and retain their assertions. Portable policy tests should not inherit those imports. Do not simply ignore/skip backend failures or silently pass when a required host capability is absent.
- Developer test-only assumptions: `tilo-dev/src/lib.rs:9535` `/bin/sh`, `:10311` permission assertions, `:11746` Unix symlink cfg, `:11896`–`:11937` executable shell fixtures; `src/main.rs:1824` `/tmp` comparison fixture. `ui-builder/src/main.rs:65` permissions tests are guarded correctly. Use platform-specific test modules or a tiny test executable if portability is required.
- Release catalogue tests `tilo-model-catalog.rs:812` onward assert Unix key permissions; keep secure-file behavioral tests tied to actual production backend, adding native ACL equivalents if supported.

## Migration order and adjacent ownership

1. Define authenticated local service-channel/owned handle and secure filesystem/location capabilities with existing launcher/atomic_file owners; preserve authority and fail-closed semantics.
2. Move Linux implementations without changing behavior; keep neutral protocol, catalogue, resource/route policy and provisioning state machines available independently.
3. Reuse ProcessSupervisor, LocalModelBackend, RuntimeRegistry, LaunchResourceBroker, SandboxExecutor and VmDelegate instead of creating parallel command interfaces.
4. Rewire service composition and target-specific dependency selection; migrate native tests intact, then validate supported host matrix.

Adjacent scope requiring coordination: `crates/packaging/launcher` fd handoff, capability plans, Landlock, NetGate and atomic_file dependency owner; `crates/windows/windows-vm` RAIL/SPICE/VM interfaces; system-extension deployment/service; OS units/runner packaging; shell service callers; app-framework users are excluded from this audit and should consume the eventual semantic API without this report reviewing their internals.

**Summary:** 10 grouped findings, including deliberate backends and host tools. Highest-risk extraction points are authenticated inherited IPC, nofollow/private filesystem invariants, Bubblewrap plans/handles, and descendant/audio lease lifecycle. No builds or source edits performed.
