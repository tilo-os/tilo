# A08 — Runtime, session, and packaging platform extraction

## Scope and method

Scope: source, manifests, build scripts, tests, fixtures, examples, and deployment under `crates/runtime/`, `crates/session/`, and `crates/packaging/`. The audit covers target guards, native APIs and dependencies, FFI, commands, host directories, environment discovery, sockets, and representative security-module entry points. Application-framework internals and the vendor NTFS archive are outside its scope. Findings identify extraction work rather than observed runtime failures.

## Destination alignment

Read `docs/architecture/platform-services.md` (proposed, implementation not started). All suggested platform capabilities below mean narrowly scoped implementation crates under `crates/system` or `crates/runtime`, optionally exposed through `ui::platform` for apps. Headless daemons/CLIs should depend directly on these service crates, never acquire renderer dependencies. Existing authorities such as runtime-portal, NativeGrantAuthority/FsSession, SecretProvider, os-actions and bounded-command should be reused. Many audited services already occupy the correct architectural layer: extracting their native submodules or fixing consumer dependencies is preferable to gratuitous wholesale crate relocation. No framework-to-app dependency or publicly exposed unsafe primitive is proposed. Preserve formats/protocols and a minimal reviewed lockfile diff; future final compile gate is `cargo check -p shell --tests --locked`.

## Actionable findings

**Current-tree note:** The `crates/packaging/hello/*`,
`crates/packaging/notes-cli/*`, `crates/session/tilo-power/*`, and
`vendor/ntfs-3g-source.tar.gz` locations in A08-13, A08-24, and the candidate
appendix are historical source anchors. No current one-to-one counterpart was
verified for those demo/daemon/archive paths. Retain the packaging and power
architecture findings, but do not treat those specific locations as live
source references.

**Source-anchor note:** Verified prefix repairs identify current moved paths where
available. Any `:line` suffix remains the original 2026-09-18 audit coordinate; it
is historical evidence and does not assert that the cited symbol is at that line
in the current tree.

### A08-01 — Service discovery and connected-handle transport

**Evidence:** `crates/runtime/runtime-portal/src/lib.rs:38` exports `RawFd` (an i32 placeholder off Unix); `:86` discovers env FDs; `:126` exposes `UnixCapnpClient`. `src/fd_transport.rs:3` implements SCM_RIGHTS send/receive; `src/native.rs:34` has explicit unsupported non-Unix attachment. Clients: `crates/runtime/runtime-collab/src/lib.rs`, `crates/runtime/runtime-fs/src/lib.rs:99`, `crates/runtime/runtime-identity/src/broker.rs:8`, `crates/packaging/app-host/src/client.rs`.

**Platform/classification/impact:** Unix backend intentionally inherited by shared runtime APIs; extraction candidate, especially public raw-descriptor ownership and environment discovery. Existing non-Unix portal stubs do not by themselves make the runtime umbrella portable.

**Boundary/migration:** retain Cap'n Proto schemas/domain clients; introduce an owned authenticated service connection capability and platform service-discovery implementation. Unix implementation owns descriptor validation, CLOEXEC, SCM_RIGHTS and tokio conversion. Preserve process-owner PID check, one-time ownership transfer, independent service handles, framing bounds and FD cleanup. Reuse runtime-portal as the current central transport seam rather than invent service-specific copies.

**Acceptance:** same client tests against injected transport; wrong/stale descriptors rejected, inherited metadata never grants authority to subsequent children, surplus/malformed ancillary FDs closed, absent service distinguishable from malformed supplied authority; unsupported platforms cannot silently open ambient sockets.

### A08-02 — Automatic native attachment mixes bootstrap policy with Unix exec/build machinery

**Evidence:** `crates/runtime/runtime-portal/src/native.rs:53` Unix module; `:137` execs qos-runtime-mcp, `:160` executable-bit probe, `:201` resolves development Cargo target, `:211` creates O_NOFOLLOW/mode-0600 flock-protected build lock. Remaining same-file code supervises developer builds and verifies cached output.

**Classification:** native host backend plus development tooling embedded in SDK. macOS/Linux supported; other hosts explicitly Unsupported.

**Boundary/migration:** separate attachment policy from `NativeRuntimeBootstrap` (locate trusted host, validate inherited connection, attach/resume). Keep optional build-on-first-run as development provider, with lock ownership/nlink checks and bounded child lifetime. Avoid a generic spawn fallback that changes exec/identity semantics.

**Acceptance:** packaged launch never unexpectedly compiles; stale PID/env handoffs reattach safely; invalid explicitly supplied host remains an error; concurrent first launches compile once; failed/timeout builds do not leak children or lock authority.

### A08-03 — Granted filesystem implementation is a valuable existing security seam

**Evidence:** `crates/runtime/runtime-fs/src/lib.rs:64` gates grants/native/operations/recovery on Unix; `src/native.rs:11`, `:164` use Unix path bytes and device/inode identity; `src/grants.rs:4`; `src/operations.rs:630`, `:638`, `:1116`, `:1130`, `:1447` use fsync, rename NOREPLACE, no-follow relative opens, exclusive creates and unlinkat; `src/recovery.rs:164`, `:175`, `:271`, `:286` use directory capabilities, exclusive journal lock, atomic receipt publication. `cap-std` and `rustix` in Cargo.toml.

**Classification:** intentional Unix/macOS/Linux backend currently mixed with domain operation and recovery state. Do not replace with ambient `PathBuf` I/O.

**Boundary/migration:** keep `NativeGrantAuthority`/`FsSession`, opaque grants, owner/mount IDs, generations, revocation, pagination and receipt semantics as domain contracts. Extract object identity, child-name representation, no-follow handle-relative open, atomic no-replace rename, durable receipt storage, locking, metadata/time and streaming read/write primitives. A backend must attest support for each required operation; Unsupported is preferable to check-then-rename emulation.

**Acceptance:** existing native grant/operation/recovery tests run unchanged against production backend; symlink swaps, rename replacement, XDEV, revoked parent, stale row identity, concurrent recovery, cancellation and crash publication preserve existing outcomes. Foreign filename encodings round-trip without granting arbitrary path traversal.

### A08-04 — Storage service escapes the guarded native filesystem boundary

**Evidence:** `crates/runtime/runtime-fs/src/lib.rs:81` unconditionally exports storage, while `src/storage.rs:9` imports UnixStream and `:12` fixes `/run/tilo/fs-storage.sock`.

**Classification/impact:** leaked Unix transport/system daemon locator in otherwise partially guarded SDK. Source-level non-Unix dependency gap, not a tested compiler failure.

**Migration:** keep storage status/eviction request types and bounded frame codec; route through authenticated service discovery from A08-01 and a platform storage-service locator. Do not treat arbitrary user path as equivalent to trusted system storage service.

**Acceptance:** unsupported backend explicitly unavailable; Unix endpoint unchanged; frame-size and identity checks preserved; no new ambient socket bypass.

### A08-05 — XDG portals are a deliberate backend, not the portable capability API

**Evidence:** `crates/runtime/runtime-portal-xdg/Cargo.toml:22` ashpd dependency; `src/lib.rs:85` DBUS_SESSION_BUS_ADDRESS heuristic, `:169` file chooser, `:252` screencast, `:319` notifications, `:377` secrets, `:405` open URI. `crates/runtime/runtime/Cargo.toml` unconditionally includes this backend.

**Classification:** intentional Linux/freedesktop provider; shared umbrella dependency leak.

**Migration:** expose semantic file selection/save, capture authorization/session, notification and external URI APIs through platform; preserve cancel vs unavailable vs no-response. XDG implementation remains ashpd-backed. Do not expose PipeWire node IDs as universal capture tokens, and retain lifetime/authorization of portal sessions. Secret retrieval is distinct from host identity key storage A08-06.

**Acceptance:** no D-Bus dependency required merely to import portable runtime; cancellation is not an error/fallback grant; native alternatives return equivalent scoped resources and Unsupported where no provider exists.

### A08-06 — Identity private storage has an existing provider contract to reuse

**Evidence:** `crates/runtime/runtime-identity/src/secure_store.rs:16` Unix private file primitives, `:44` `SecretProvider`, `:70` secret-tool backend, `:179` BrokerFileProvider; `src/lib.rs:263` private atomic write, `:324` directory discovery, `:363` ownership/private-directory checks, `:390` IdentityLock, `:1741` private record read; `src/backup.rs:525`, `:607`, `:644` Linux renameat2 vs hard-link publication. `src/broker.rs:8`, `src/lib.rs:49` Unix signing transport.

**Classification:** deliberate host security implementations embedded in shared identity lifecycle logic. Non-Linux error translation exists; Unix assumptions remain.

**Migration:** implement platform secret storage via existing SecretProvider; move private file/ACL validation, atomic create-without-overwrite, durable replacement, locking, and config locations behind platform services. Preserve explicit KeyProtection and backend IDs, immutable locator/value identity, zeroization, locked-keyring errors, and broker-only access. A keychain unavailable error must not silently re-create or downgrade existing protected records. Separate broker transport per A08-01.

**Acceptance:** no key path/material enters sandbox; malicious symlink/hardlink/wrong-owner files rejected; backup destination never overwritten; concurrent selector/key changes serialize; new platform protection level accurately represented and round-trip import/export verified.

### A08-07 — ush is an entire intentional Linux runtime backend

**Evidence:** `crates/runtime/ush/src/guest.rs:13` overlay filesystem design and `:577` user namespace procfs handles; `src/profile.rs:102` Linux syscall deny lists; `src/guard.rs:119` Landlock probing; `src/layer.rs:119` statfs and `:335` Unix file types; `src/supervisor.rs:456` systemd inherited listener, `:589` PTY/session handling; `src/main.rs:280` filesystem confinement and `:709` PID signalling. Supporting `src/{lib,store,broker,pkg,policy}.rs` embed Linux guest paths, Fedora RPM/libdnf layout, socket permissions and executable metadata. Cargo manifest uses unconditional landlock/seccompiler/libc.

**Classification:** retain as Linux backend, not a reason to translate syscalls into nominal portable no-ops.

**Migration:** surface `ShellSession`/`ShellGuestPolicy` capabilities (host base read-only, persistent/ephemeral overlay layers, authorized directory grants, PTY resize/exit, audit stream). Backend selection must reject unavailable confinement. Keep dsh/ush privilege profiles and subordinate-ID requirements separate; package transaction inspection/quarantine is backend-specific. Reuse bounded-command for bounded external tools.

**Acceptance:** Linux integration tests exercise actual confinement, read-only host, seccomp, revocation and PTY lifecycle; no raw host shell fallback; guest cleanup reaps namespace helper and removes only owned runtime artifacts. Other backends advertise Unsupported until equivalent security exists.

### A08-08 — Launcher security machinery must move as a coherent Linux backend

**Evidence:** `crates/packaging/launcher/src/lib.rs:44` and following module guards already isolate run/sandbox/host/Landlock/D-Bus proxy on Linux. `src/sandbox.rs:400` constructs bwrap isolation; `src/run.rs:929` remaps service descriptors, `:979` applies limits, `:1386` spawn/wait, `:2362` sealed seccomp staging; `src/landlock_shim.rs:298` close_range, `:332` fork and `:445` exec; `src/seccomp.rs:56` architecture-specific policy; `src/landlock.rs:64` fstat. Cargo target dependencies already Linux-specific.

**Classification:** intentional backend with reasonably explicit cfg boundary; retain low-level code behind platform.

**Migration:** shared launcher should produce a semantic LaunchPlan containing evaluated capabilities, workspace/app storage, bounded capture, trusted runtime, services and display grants. Linux backend compiles plan to bwrap/Landlock/seccomp/env/FD lifecycle. Keep capability policy (`capabilities.rs`, shared collab types) and manifest validation out of OS adapter. Do not make successful sandbox launch synonymous with successful process spawn.

**Acceptance:** fail closed when bwrap/Landlock/seccomp or required isolation unavailable; inherited FDs/env closed/cleared; memory/process/output/time limits and exit status maintained; existing room_confinement/sandbox_environment/netgate tests remain production-path tests.

### A08-09 — Launcher shared modules still expose Unix IPC and Linux environment plans

**Evidence:** `crates/packaging/launcher/src/lib.rs:54` unconditionally exports netgate and `:63` services; `src/netgate.rs:110` UnixStream/Listener; `src/services.rs:33`, `:145`, `:182`, `:351` FD binds, runtime paths and authenticated handoffs. `src/appdir.rs:97`, `:284`, `:297` XDG storage and bwrap args; `src/profile.rs:103` fixed Linux mount profile. `src/host.rs:147` detaches unpackaged launches with setsid.

**Classification:** leaked Unix mechanism in exported shared launcher modules, alongside intentional Linux plan helpers.

**Migration:** separate shared destination/egress policy from Unix forwarding; service grants become owned authenticated capabilities, not `(socket path, fd integer)` payloads. Keep app data semantic locations independent of mount argv. Host-app launch is explicitly a separate trusted unpackaged mode, never fallback for package sandbox failure.

**Acceptance:** egress allowlist/address restrictions, authenticated app/workspace handoff and private service endpoints preserved; generic policy crate has no Unix imports; non-Linux package launch cannot silently use host mode.

### A08-10 — GPU selection, runtime hardening and performance are host adapters

**Evidence:** `crates/packaging/launcher/src/gpu.rs:59`, `:161`, `:314` Linux DRM/sysfs/ICD discovery and `:336` driver selection; `src/hardening.rs:46` fixed scudo `.so`, `:97` runtime lookup; `src/performance.rs:56`, `:84`, `:95` sched_attr/prctl/ioprio and `:134` non-Linux return; `src/sandbox.rs:211` turns GPU selection into mounts.

**Classification:** intentional Linux device/runtime backend; semantic structures can remain shared. Non-Linux performance hints are currently best effort, unlike mandatory confinement.

**Migration:** `RenderDeviceGrant` resolves device plus exact driver dependencies without broad `/dev/dri` access; runtime hardening profiles report enforced/unavailable protections; performance hints remain explicitly advisory and separate from security requirements.

**Acceptance:** compositor-selected device remains selected, only authorized DRM capabilities exposed, fallback renderer supported without broad grants; no glibc/scudo variables injected into unrelated native loaders; unsupported hints do not masquerade as security enforcement.

### A08-11 — App-host daemon mixes package operations, Linux launchers and service-manager plumbing

**Evidence:** `crates/packaging/app-host/src/main.rs:108` LISTEN_PID/FDS and fd3; `src/lib.rs:98` XDG_RUNTIME_DIR/temp fallback; `src/transport.rs:12`, `:27`, `:80` Unix listener/private permissions/handoff; `src/service.rs:27` system payload paths, `:252` launcher executable, `:614`, `:657`, `:696`, `:753` launcher/flatpak commands. `systemd/tilo-app-host.{service,socket}` and sd-notify dependency.

**Classification:** daemon/platform adapter mixed with portable content-addressed store/registry/GC semantics. Socket permissions are currently best-effort (`transport.rs:37`); migration should define fail-closed private endpoint requirements rather than reproduce silent setup failure.

**Migration:** move session daemon activation/readiness, listener ownership and private endpoint location into platform; route native/Flatpak execution through explicit package backend capability; retain install/resolve/GC logic. Reuse inherited listener validation (matching PID, exactly one FD, CLOEXEC) rather than generic fd3 assumptions.

**Acceptance:** activation tests exercise descriptor validation and ownership; endpoint permission failure is observable; callers cannot bypass app handoff or launch policy by choosing a socket path; stop/cleanup handles only daemon-owned files.

### A08-12 — app-fs is an intentional FUSE adapter over reusable verified content storage

**Evidence:** `crates/packaging/app-fs/Cargo.toml:14` fuser; `src/store.rs:19` Unix pread; `src/fs.rs:191` uid/gid; `src/mount.rs:24` FUSE allow_other policy; `src/bin/tilo-app-mount.rs:118` signal handlers.

**Classification:** retain target-specific mount backend; extract reusable collection/blob/cache logic where independent.

**Migration:** platform `PackageProjection` should expose verified immutable package files and lifecycle, preserving demand reads; FUSE implementation owns errno/inodes/uid/gid/mount options/signals. Do not automatically enable allow_other on another backend to approximate filesystem reachability.

**Acceptance:** hash verification/demand reads and read-only behavior unchanged; unmount on termination; unrelated users denied by default; lack of FUSE is Unsupported or an explicit separately reviewed projection strategy.

### A08-13 — Runtime bundles are Linux ELF/glibc artifacts, not generic host runtimes

**Evidence:** `crates/packaging/runtime-bundle/src/lib.rs:166`, `:191`, `:202`, `:224` Linux loaders/libc/ld.so.cache; `:266` `/usr/lib/tilo-runtime`; `crates/runtime/runtime-dylib/Cargo.toml:7` `.so` thin SDK and broad native service dependencies; `crates/packaging/hello/build-thin.sh:42` distrobox wrapper and `:44` GNU stat. `hello/demo.sh` supplies Linux pipeline demo.

**Classification:** intentional Linux artifact builder/deployment demos. Do not rename `.so` strings and imply Mach-O/PE support.

**Migration:** keep bundle hashes/signatures/manifest model shared; explicit target ABI provider chooses loaders/dependencies/format and supported architecture. Target-gate runtime-dylib dependency graph rather than importing every Linux service on every host. Demos stay labelled target-specific; developer container selection belongs to tooling configuration.

**Acceptance:** bundle resolved by target+ABI and verified before use; cross-host packaging never accidentally copies build-host libc; Linux thin linking and hardening survive; native loader support added only with independent validation.

### A08-14 — CPU capability detection is duplicated and inconsistent

**Evidence:** `crates/packaging/install/src/isa.rs:20` reads `/proc/cpuinfo` and `:75` checks full feature sets; `crates/packaging/app-registry/src/index.rs:173` uses Rust feature detection with fewer required predicates for named x86/ARM levels, and unsupported architectures fall back to x86 baseline.

**Classification/impact:** leaked Linux feature discovery plus duplicated shared target-selection policy. On non-Linux `/proc` absence gives baseline; differing detectors can select different variants on the same feature set. No runtime execution failure tested.

**Migration:** one platform CPU capability snapshot (architecture, usable feature set, OS state support), one shared mapping to package ISA levels; unknown architecture explicitly unsupported. Retain parser as Linux adapter test utility.

**Acceptance:** identical synthetic feature vectors yield identical install and registry selection; missing required feature never chooses higher variant; process/emulation architecture distinguished from physical CPU where necessary.

### A08-15 — Recovery must not equate unavailable process observation with death

**Evidence:** `crates/packaging/install/src/main.rs:1437` skips live transaction owner, `:1478` Linux `/proc/<pid>` probe, `:1483` every non-Linux PID returns false.

**Classification/impact:** concrete unsafe portability fallback in update recovery: on a non-Linux build, a concurrently live transaction can be classified recoverable. PID reuse also requires consideration when designing the contract.

**Migration:** platform process-instance liveness or, preferably, held transaction lease/lock with dead-owner recovery; represent Unknown separately and refuse recovery if liveness cannot be established. Keep rollback/commit reconciliation domain logic shared.

**Acceptance:** two installer processes cannot recover each other's active transactions; crash recovery works; unsupported observation fails rather than claims dead; reused PID does not indefinitely block or authorize wrong transaction.

### A08-16 — Cross-process registry locking disappears on non-Unix targets

**Evidence:** `crates/packaging/app-catalogue/src/client.rs:273` checkpoint lock only under cfg(unix), implementation `:295`; `crates/packaging/registry-server/src/moderation.rs:529` Unix ProcessGuard uses flock, while `:580` non-Unix guard takes only PROCESS_MUTEX.

**Classification/impact:** leaked OS synchronization with semantically weaker fallback. Atomic-file replacement cannot make read-validate-write transactions atomic across processes; concurrent checkpoint writes may regress persisted anti-rollback state, and independent reviewer/server processes can race moderation transitions.

**Migration:** platform exclusive durable-store lock/transaction contract, shared by all writers; unsupported locking returns explicit failure. Reuse atomic-file for publication inside the transaction, not as replacement for locking.

**Acceptance:** multi-process races preserve monotonic checkpoint sequence and one authorized moderation transition; locks release on crash; no platform accepts process-local mutex as cross-process protection.

### A08-17 — Package path/executable and private-key semantics need platform-aware validation

**Evidence:** `crates/packaging/pkg/src/unpack.rs:144` checks `Path::is_absolute` and slash-separated `..`, `:160` cfg(unix) sets executable bits; `crates/packaging/install/src/shim.rs:167` Unix execute permissions; `crates/packaging/cli-shim/src/main.rs:50`, `:138` fixed launcher/execv; `crates/packaging/pkg-cli/src/main.rs:710` and `crates/packaging/runtime-bundle/src/main.rs:276` Unix key-file modes; `crates/packaging/registry-server/src/main.rs:244` signing key privacy checked only on Unix.

**Classification:** shared package format containing host filesystem/path and process assumptions. Windows path parsing is a particular review target: slash-only traversal validation does not model all host separators/prefixes. Do not assert full exploit reachability without auditing upstream collection validation.

**Migration:** canonical package-path type independent of host path parser; backend maps components and rejects separators/prefixes/reserved names that cannot be represented safely. Separate executable installation and PATH shim provider; private key reader/writer must enforce effective user-only access on each supported host or fail explicitly.

**Acceptance:** corpus covers `..`, backslashes, drive-relative/UNC/absolute paths, mixed separators, case/reserved-name collisions and symlink destinations; unpack stays within destination. Shim forwards exact args/status without shell interpolation. Private keys remain protected on every enabled target.

### A08-18 — Directory and installed-tool discovery is scattered

**Evidence:** `crates/runtime/cli/src/lib.rs:137` XDG then Windows LOCALAPPDATA then HOME fallback; `src/workspace.rs:20` `/var/workspace`; `crates/packaging/pkg/src/trust.rs:370`; `app-registry/src/config.rs:272`; `app-catalogue/src/config.rs:49` system trust pin and `:125` cache; `launcher/src/registry.rs:141`, `:171`, `:195`; `install/src/main.rs:430`, `:450`; `install/src/shim.rs:146`; `registry-server/src/lib.rs:116`; runtime-bundle locations in A08-13, identity in A08-06.

**Classification:** leaked host layout intertwined with intentional Linux sandbox virtual paths. Existing dirs crate usage is a reusable base, but semantic stores/overrides differ.

**Migration:** platform typed locations for host private state/config/cache/runtime endpoints, trusted system packages/trust roots, installed helpers; retain `/app` and `/var/workspace` as Linux guest ABI where intentional. Preserve existing explicit overrides and migration compatibility. A session-specific document XDG override must not redirect system runtime trust discovery.

**Acceptance:** host defaults correct per supported target; tests inject roots without mutating global env; trust roots never fall back to user-writable location accidentally; data remains discoverable across migration. Environment presence stays heuristic, never authority.

### A08-19 — Greeter login/session discovery and power commands are Linux session providers

**Evidence:** `crates/session/greeter/src/greetd_client.rs:31` Unix greetd IPC; `src/local_users.rs:25` passwd/login.defs/shells; `src/sessions.rs:108` wayland session desktop files; `src/greeter_app.rs:1257` systemctl and `:1327` Wayland XDG env; `src/auth.rs` existing auth state machine.

**Classification:** deliberate Linux login screen with OS operations inside UI orchestration (implementation included because this is session service, not excluded toolkit).

**Migration:** reuse auth state machine over platform LoginSession provider: enumerate eligible accounts/sessions, begin/challenge/respond/cancel, start authenticated session, authorized power action. greetd and desktop-file parsing remain Linux provider. Move systemctl out of widget/application handler.

**Acceptance:** passwords zeroized and never logged; challenge/order/cancel semantics preserved; session starts only after authenticated success; power authorization continues through OS policy; unsupported host login cannot be simulated as success.

### A08-20 — In-session unlock is a Linux PAM privileged helper

**Evidence:** `crates/session/tilo-unlock-helper/src/pam.rs:161` dlopen libpam (string immediately preceding), `:112` PAM conversation allocation; `src/main.rs:43` Tilo PAM service; Cargo.toml documents libpam.so.0.

**Classification:** intentional privileged Linux security backend.

**Migration:** platform SessionUnlockVerifier capability returns verified/rejected/unavailable for explicitly selected local session identity. Linux provider retains helper subprocess and PAM policy; do not replace with checking a locally stored password/hash or a UI-only unlock decision. Preserve secret stdin channel, bounded input and zeroized buffers.

**Acceptance:** wrong password never unlocks; helper crash/unavailable fails closed; account/PAM policy checked; secrets absent from argv/env/logs; timeout/cancel cleans helper.

### A08-21 — Installer inventory and install execution belong to Fedora Bootc deployment provider

**Evidence:** `crates/session/installer/src/disks.rs:475`, `:529`, `:575` Linux block naming, protected mounts and lsblk; `src/install.rs:69`, `:139`, `:153`, `:275`, `:464` process groups and fixed privileged helpers with bounded stream; `src/media_profile.rs:12`, `:201` run-directory profile/zoneinfo; `src/timezones.rs:45`, `:71` timedatectl/zoneinfo. `src/installer_network.rs:54` already wraps os-actions through NetworkBackend; `src/power_readiness.rs` already consumes power service.

**Classification:** target-specific deployment retained; OS access should be outside portable screen/state logic.

**Migration:** install backend exposes authoritative inventory, immutable selection/preflight tokens, plan, progress, cancellation and authorized commit. Keep disk identity, live-system/holder exclusion, alongside validation, destructive confirmation and helper policy in Linux Bootc implementation. Reuse existing NetworkBackend/os-actions and power capability rather than duplicate NetworkManager/sysfs. Timezone catalogue/default-layout provider separate from widget code.

**Acceptance:** stale disk selections revalidated, no live media/system disk wipe, failed inventory not equivalent to safe disk, bounded output/cancellation kills process group, secrets never on command line; unsupported target reports installer unavailable rather than invoking host tools.

### A08-22 — NTFS broker/parser boundary must remain stronger than a generic file reader

**Evidence:** `crates/session/qos-ntfs-helper/src/broker.rs:6` explicitly rejects non-Linux production claims; `src/broker/linux.rs:56` sysfs physical-device topology and `:115` mountinfo; `src/broker/ipc.rs:43`, `:105` protected /run endpoint and accept4; `src/broker/session.rs:42` getrandom; `native/sandbox.c:29` architecture-checked syscall allowlist and `:65` descriptor-only child setup. `src/reader.rs:8` pinned C FFI. `crates/session/qos-ntfs-protocol/src/lib.rs:161` correctly gates socket/client on Linux; `src/socket.rs:18`, `:44` seqpacket and SO_PEERCRED, `:117` rejects ancillary/truncation.

**Classification:** intentional Linux administrative/security backend plus Unix/macOS fixture-reader support; protocol types/NTFS UTF-16 names are format semantics, not leaked Windows assumptions.

**Migration:** keep production device claim authority isolated from serializable facts; expose a broker-issued read session capability bound to device identity, principal and deadline. Linux implementation owns topology revalidation, root privilege drop, read-only FD, syscall sandbox, peer credentials, child death/lifetime and limits. Never let caller-provided path/health/FD assertions construct authority. Keep pure protocol/manifest parser logic shared.

**Acceptance:** physical topology changes invalidate claim; mounted/held/unsafe devices refused; descriptor swaps and ancillary injection rejected; parser has only admitted read FD and output FD; non-Linux production mode remains Unsupported until equivalent confinement and claims are implemented.

### A08-23 — NTFS native build and staging are explicit target tooling; vendor is not extraction fodder

**Evidence:** `crates/session/qos-ntfs-helper/build.rs:3` Unix family check, `:6` CC/AR, `:76` macOS iconv; `build_native.py:20` Homebrew PATH/ACLOCAL, `:34` QOS_NTFS_HOST, checksum-before-extract and forbidden device-adapter symbol inspection; `stage_install.py`; native `ntfs_reader.c`, `strict_logfile.c`, `ntfs_reader.h`; `vendor/ntfs-3g-source.tar.gz`, lock/README/COPYING.

**Classification:** intentional pinned vendor and native build/deployment tooling. Homebrew paths are developer-host assumptions, not product runtime paths.

**Migration:** keep vendored hash/pin/license and offline build; let build tooling derive/accept compiler/archiver/configure target independently from build host. Move first-party sandbox adapter to target backend; do not mass-refactor upstream NTFS source. Preserve forbidden default device IO adapter check.

**Acceptance:** native archive matches requested target; changing compiler/host demands distinct output or rejects stale config; offline checksums and symbol checks remain mandatory; Linux privileged staging validates ownership/modes and never converts fixture mode to production claims.

### A08-24 — Power governor daemon is a Linux image component

**Evidence:** `crates/session/tilo-power/src/main.rs:58`, `:63`, `:65`, `:117` Unix notify, sysfs AC/cpufreq, override lock; absence of AC path currently interpreted as AC (`:88`).

**Classification:** retain target-specific daemon; avoid duplicate portable implementation beside existing `crates/system/power` services (adjacent owner should coordinate).

**Migration:** platform host power/performance provider owns power-source observation/governor write/support detection. Keep distro-specific policy and systemd daemon in Linux image; distinguish unknown source from unsupported hardware when broadening beyond current fixed AC path.

**Acceptance:** multiple AC/battery device layouts supported or explicitly unknown; override lock honored; writes constrained to detected CPU governor paths; daemon readiness and privilege policy preserved.

## Intentional backends, tests, deployment, and false positives

- Linux-only sandbox, PAM, greetd, Bootc installer, FUSE, ELF runtime bundle, NTFS production broker and systemd units should remain target implementations. Platform extraction means isolating them, not making every operation succeed on every host.
- `crates/packaging/{pkg,registry-types}/` cryptographic manifests, signatures and wire constants; `app-registry` fetch/verify; `runtime-collab` schemas/client methods; generic caches, GC, manifests and UI layout helpers are predominantly domain code. Their mentioning Linux example paths does not itself require moving them.
- Generated Cap'n Proto build scripts in runtime-fs/runtime-collab/app-host invoke capnpc: host build-tool requirement, not target Unix behavior. Keep generated schema logic shared; ensure build tool available independently of target runtime.
- `crates/packaging/app-host/systemd/*`, `registry-server/systemd/*`, hello demos and NTFS staging scripts are deployment/tooling, not portable runtime API. Preserve explicit target labels.
- Test-only assumptions appear throughout native FD/socket tests, launcher Linux subprocess tests, ush namespaces/PTY tests, NTFS Python coordinators and fixture C helpers, FUSE mount tests, `/bin/sh`/`/dev/zero` installer stream tests, and Unix symlink/permission fixture construction. Target-gate backend integration tests, preserve shared policy tests across hosts, and run required Linux backend tests in a environment that actually supplies their subject. Do not convert missing infrastructure into passing verification. Existing `ush/src/layer.rs:539` test prints and returns when /dev/shm absent; treat as coverage gap, not proof of production failure.
- No tests/builds were run per audit authorization. Proposed acceptance criteria are future implementation requirements.

## Suggested implementation order

1. Introduce semantic platform contracts for owned service connections, private state/locks, process/transaction leases and CPU capability discovery; preserve existing APIs as compatibility wrappers.
2. Address A08-15/A08-16 and audit A08-17 path validation before enabling further targets. These change security/correctness semantics, not just import placement.
3. Move cohesive Linux launcher/ush/session/NTFS/FUSE/ELF backends with their tests and target dependencies. Keep shared grants, policy, package schemas and cryptographic validation outside.
4. Add native providers one capability at a time, with explicit Unsupported and production-path security conformance tests. Do not promise equivalent sandboxing based on compile-only success.

## Adjacent scope

Coordinate with platform owner, `crates/system/{atomic-file,bounded-command,accounts,os-actions,power,desktop-entry}`, `crates/collab/` identity owners, `crates/session/shell/` native runtime/brokers and launcher callers, root Cargo dependency/target feature management, `os/` image units/helper paths, `crates/system/fs/` storage service, and excluded UI callers. No edits in those scopes were made.

## Per-file lexical coverage appendix

The following inventory lists files hit by the broad search with a representative one-based anchor and hit count. It includes comments, tests, and fixtures and is not a list of independent defects. Files without matches are not claimed to be platform-neutral.

- `crates/packaging/app-catalogue/Cargo.toml:18` — 2 candidate lines.
- `crates/packaging/app-catalogue/examples/serve-test-registry.rs:12` — 6 candidate lines.
- `crates/packaging/app-catalogue/examples/support/authoring.rs:175` — 2 candidate lines.
- `crates/packaging/app-catalogue/src/client.rs:236` — 11 candidate lines.
- `crates/packaging/app-catalogue/src/config.rs:3` — 5 candidate lines.
- `crates/packaging/app-catalogue/src/install.rs:364` — 7 candidate lines.
- `crates/packaging/app-catalogue/src/lib.rs:117` — 11 candidate lines.
- `crates/packaging/app-catalogue/tests/registry_to_installed.rs:134` — 9 candidate lines.
- `crates/packaging/app-fs/src/bin/tilo-app-mount.rs:48` — 5 candidate lines.
- `crates/packaging/app-fs/src/cache.rs:171` — 1 candidate lines.
- `crates/packaging/app-fs/src/collection.rs:164` — 1 candidate lines.
- `crates/packaging/app-fs/src/fs.rs:191` — 2 candidate lines.
- `crates/packaging/app-fs/src/mount.rs:24` — 2 candidate lines.
- `crates/packaging/app-fs/src/store.rs:4` — 3 candidate lines.
- `crates/packaging/app-fs/tests/mount.rs:11` — 3 candidate lines.
- `crates/packaging/app-host/Cargo.toml:25` — 3 candidate lines.
- `crates/packaging/app-host/build.rs:6` — 1 candidate lines.
- `crates/packaging/app-host/schema/app-host.capnp:5` — 3 candidate lines.
- `crates/packaging/app-host/src/gc.rs:5` — 6 candidate lines.
- `crates/packaging/app-host/src/lib.rs:96` — 3 candidate lines.
- `crates/packaging/app-host/src/main.rs:20` — 15 candidate lines.
- `crates/packaging/app-host/src/registry.rs:6` — 16 candidate lines.
- `crates/packaging/app-host/src/service.rs:27` — 16 candidate lines.
- `crates/packaging/app-host/src/store.rs:8` — 1 candidate lines.
- `crates/packaging/app-host/src/transport.rs:28` — 2 candidate lines.
- `crates/packaging/app-host/systemd/tilo-app-host.service:8` — 1 candidate lines.
- `crates/packaging/app-host/systemd/tilo-app-host.socket:6` — 1 candidate lines.
- `crates/packaging/app-host/tests/cap_n_proto_round_trip.rs:34` — 2 candidate lines.
- `crates/packaging/app-host/tests/socket_activation.rs:5` — 12 candidate lines.
- `crates/packaging/app-registry/Cargo.toml:15` — 1 candidate lines.
- `crates/packaging/app-registry/src/config.rs:5` — 4 candidate lines.
- `crates/packaging/app-registry/src/index.rs:174` — 10 candidate lines.
- `crates/packaging/app-registry/src/publish.rs:254` — 1 candidate lines.
- `crates/packaging/app-registry/src/qpkg.rs:235` — 3 candidate lines.
- `crates/packaging/app-registry/src/transport.rs:206` — 1 candidate lines.
- `crates/packaging/cli-shim/src/main.rs:50` — 7 candidate lines.
- `crates/packaging/hello/Cargo.toml:14` — 3 candidate lines.
- `crates/packaging/hello/build-thin.sh:1` — 9 candidate lines.
- `crates/packaging/hello/demo.sh:1` — 7 candidate lines.
- `crates/packaging/hello/src/main.rs:17` — 9 candidate lines.
- `crates/packaging/install/Cargo.toml:21` — 2 candidate lines.
- `crates/packaging/install/src/integrity.rs:278` — 5 candidate lines.
- `crates/packaging/install/src/isa.rs:10` — 6 candidate lines.
- `crates/packaging/install/src/main.rs:19` — 18 candidate lines.
- `crates/packaging/install/src/shim.rs:132` — 11 candidate lines.
- `crates/packaging/install/src/state.rs:68` — 3 candidate lines.
- `crates/packaging/install/tests/lifecycle.rs:8` — 6 candidate lines.
- `crates/packaging/install/tests/runtime_cli.rs:4` — 28 candidate lines.
- `crates/packaging/install/tests/trust.rs:14` — 4 candidate lines.
- `crates/packaging/launcher/Cargo.toml:38` — 5 candidate lines.
- `crates/packaging/launcher/src/appdir.rs:5` — 27 candidate lines.
- `crates/packaging/launcher/src/capabilities.rs:22` — 14 candidate lines.
- `crates/packaging/launcher/src/dbus_proxy.rs:18` — 36 candidate lines.
- `crates/packaging/launcher/src/gpu.rs:6` — 55 candidate lines.
- `crates/packaging/launcher/src/hardening.rs:31` — 9 candidate lines.
- `crates/packaging/launcher/src/host.rs:21` — 8 candidate lines.
- `crates/packaging/launcher/src/landlock.rs:22` — 6 candidate lines.
- `crates/packaging/launcher/src/landlock_shim.rs:269` — 16 candidate lines.
- `crates/packaging/launcher/src/lib.rs:43` — 6 candidate lines.
- `crates/packaging/launcher/src/main.rs:17` — 12 candidate lines.
- `crates/packaging/launcher/src/manifest.rs:433` — 10 candidate lines.
- `crates/packaging/launcher/src/netgate.rs:70` — 16 candidate lines.
- `crates/packaging/launcher/src/performance.rs:5` — 46 candidate lines.
- `crates/packaging/launcher/src/profile.rs:22` — 47 candidate lines.
- `crates/packaging/launcher/src/registry.rs:4` — 17 candidate lines.
- `crates/packaging/launcher/src/run.rs:8` — 112 candidate lines.
- `crates/packaging/launcher/src/sandbox.rs:19` — 140 candidate lines.
- `crates/packaging/launcher/src/seccomp.rs:29` — 102 candidate lines.
- `crates/packaging/launcher/src/services.rs:4` — 57 candidate lines.
- `crates/packaging/launcher/tests/fixtures/roomtest/src/main.rs:59` — 2 candidate lines.
- `crates/packaging/launcher/tests/launcher_smoke.rs:29` — 52 candidate lines.
- `crates/packaging/launcher/tests/netgate_egress.rs:11` — 32 candidate lines.
- `crates/packaging/launcher/tests/room_confinement.rs:40` — 35 candidate lines.
- `crates/packaging/launcher/tests/sandbox_environment.rs:45` — 11 candidate lines.
- `crates/packaging/notes-cli/Cargo.toml:18` — 3 candidate lines.
- `crates/packaging/notes-cli/src/main.rs:16` — 3 candidate lines.
- `crates/packaging/pkg-cli/src/main.rs:710` — 5 candidate lines.
- `crates/packaging/pkg-cli/tests/cli.rs:73` — 7 candidate lines.
- `crates/packaging/pkg/Cargo.toml:22` — 1 candidate lines.
- `crates/packaging/pkg/src/blobs.rs:278` — 1 candidate lines.
- `crates/packaging/pkg/src/lib.rs:99` — 1 candidate lines.
- `crates/packaging/pkg/src/manifest.rs:138` — 2 candidate lines.
- `crates/packaging/pkg/src/trust.rs:359` — 3 candidate lines.
- `crates/packaging/pkg/src/unpack.rs:160` — 3 candidate lines.
- `crates/packaging/pkg/src/validate.rs:59` — 3 candidate lines.
- `crates/packaging/pkg/tests/app_manifests.rs:41` — 1 candidate lines.
- `crates/packaging/pkg/tests/manifest.rs:130` — 4 candidate lines.
- `crates/packaging/registry-server/Cargo.toml:24` — 1 candidate lines.
- `crates/packaging/registry-server/src/allowlist.rs:140` — 1 candidate lines.
- `crates/packaging/registry-server/src/bundle_review.rs:319` — 3 candidate lines.
- `crates/packaging/registry-server/src/lib.rs:116` — 1 candidate lines.
- `crates/packaging/registry-server/src/main.rs:46` — 16 candidate lines.
- `crates/packaging/registry-server/src/moderation.rs:9` — 38 candidate lines.
- `crates/packaging/registry-server/src/moderation_api.rs:58` — 21 candidate lines.
- `crates/packaging/registry-server/src/storage.rs:6` — 8 candidate lines.
- `crates/packaging/registry-server/systemd/tilo-registry-server.service:3` — 3 candidate lines.
- `crates/packaging/registry-server/tests/http_end_to_end.rs:252` — 13 candidate lines.
- `crates/packaging/registry-server/tests/snapshot_cli.rs:1` — 9 candidate lines.
- `crates/packaging/registry-types/src/change.rs:28` — 7 candidate lines.
- `crates/packaging/registry-types/src/entry.rs:39` — 9 candidate lines.
- `crates/packaging/registry-types/src/handshake.rs:55` — 1 candidate lines.
- `crates/packaging/registry-types/src/moderation.rs:74` — 22 candidate lines.
- `crates/packaging/registry-types/src/snapshot.rs:31` — 18 candidate lines.
- `crates/packaging/registry-types/src/verify.rs:143` — 3 candidate lines.
- `crates/packaging/registry-types/tests/canonical_form.rs:38` — 2 candidate lines.
- `crates/packaging/runtime-bundle/Cargo.toml:12` — 2 candidate lines.
- `crates/packaging/runtime-bundle/src/lib.rs:4` — 102 candidate lines.
- `crates/packaging/runtime-bundle/src/main.rs:14` — 7 candidate lines.
- `crates/packaging/runtime-bundle/tests/roundtrip.rs:25` — 19 candidate lines.
- `crates/runtime/cli/Cargo.toml:18` — 4 candidate lines.
- `crates/runtime/cli/src/lib.rs:77` — 8 candidate lines.
- `crates/runtime/cli/src/output.rs:128` — 3 candidate lines.
- `crates/runtime/cli/src/workspace.rs:83` — 13 candidate lines.
- `crates/runtime/runtime-collab/Cargo.toml:25` — 1 candidate lines.
- `crates/runtime/runtime-collab/build.rs:4` — 1 candidate lines.
- `crates/runtime/runtime-collab/schema/collab.capnp:93` — 2 candidate lines.
- `crates/runtime/runtime-collab/src/lib.rs:296` — 6 candidate lines.
- `crates/runtime/runtime-collab/tests/mock_broker.rs:31` — 3 candidate lines.
- `crates/runtime/runtime-collab/tests/schema_v2.rs:31` — 3 candidate lines.
- `crates/runtime/runtime-collab/tests/schema_v3.rs:29` — 3 candidate lines.
- `crates/runtime/runtime-dylib/Cargo.toml:7` — 12 candidate lines.
- `crates/runtime/runtime-dylib/src/lib.rs:2` — 1 candidate lines.
- `crates/runtime/runtime-fs/Cargo.toml:28` — 1 candidate lines.
- `crates/runtime/runtime-fs/build.rs:4` — 1 candidate lines.
- `crates/runtime/runtime-fs/src/grants.rs:4` — 1 candidate lines.
- `crates/runtime/runtime-fs/src/lib.rs:64` — 12 candidate lines.
- `crates/runtime/runtime-fs/src/native.rs:11` — 8 candidate lines.
- `crates/runtime/runtime-fs/src/operations.rs:12` — 90 candidate lines.
- `crates/runtime/runtime-fs/src/recovery.rs:8` — 15 candidate lines.
- `crates/runtime/runtime-fs/src/storage.rs:9` — 3 candidate lines.
- `crates/runtime/runtime-fs/tests/mock_fs.rs:30` — 7 candidate lines.
- `crates/runtime/runtime-fs/tests/native_grants.rs:1` — 5 candidate lines.
- `crates/runtime/runtime-fs/tests/native_operations.rs:1` — 10 candidate lines.
- `crates/runtime/runtime-identity/Cargo.toml:25` — 2 candidate lines.
- `crates/runtime/runtime-identity/src/backup.rs:10` — 17 candidate lines.
- `crates/runtime/runtime-identity/src/broker.rs:8` — 4 candidate lines.
- `crates/runtime/runtime-identity/src/lib.rs:49` — 65 candidate lines.
- `crates/runtime/runtime-identity/src/secure_store.rs:12` — 10 candidate lines.
- `crates/runtime/runtime-identity/src/service_connection_tests.rs:2` — 6 candidate lines.
- `crates/runtime/runtime-portal-xdg/src/lib.rs:79` — 12 candidate lines.
- `crates/runtime/runtime-portal/Cargo.toml:29` — 1 candidate lines.
- `crates/runtime/runtime-portal/src/fd_transport.rs:3` — 46 candidate lines.
- `crates/runtime/runtime-portal/src/lib.rs:38` — 18 candidate lines.
- `crates/runtime/runtime-portal/src/native.rs:35` — 28 candidate lines.
- `crates/runtime/runtime-portal/tests/native.rs:1` — 17 candidate lines.
- `crates/runtime/runtime/Cargo.toml:20` — 5 candidate lines.
- `crates/runtime/runtime/src/lib.rs:67` — 2 candidate lines.
- `crates/runtime/ush/Cargo.toml:18` — 2 candidate lines.
- `crates/runtime/ush/src/audit.rs:46` — 7 candidate lines.
- `crates/runtime/ush/src/broker.rs:23` — 14 candidate lines.
- `crates/runtime/ush/src/guard.rs:26` — 12 candidate lines.
- `crates/runtime/ush/src/guest.rs:13` — 120 candidate lines.
- `crates/runtime/ush/src/layer.rs:13` — 24 candidate lines.
- `crates/runtime/ush/src/lib.rs:11` — 7 candidate lines.
- `crates/runtime/ush/src/main.rs:204` — 16 candidate lines.
- `crates/runtime/ush/src/pkg.rs:19` — 7 candidate lines.
- `crates/runtime/ush/src/policy.rs:177` — 9 candidate lines.
- `crates/runtime/ush/src/profile.rs:67` — 30 candidate lines.
- `crates/runtime/ush/src/store.rs:37` — 7 candidate lines.
- `crates/runtime/ush/src/supervisor.rs:31` — 36 candidate lines.
- `crates/runtime/ush/tests/serve_handoff.rs:19` — 21 candidate lines.
- `crates/runtime/ush/tests/socket_activation.rs:9` — 10 candidate lines.
- `crates/session/greeter/Cargo.toml:28` — 1 candidate lines.
- `crates/session/greeter/src/auth.rs:144` — 1 candidate lines.
- `crates/session/greeter/src/greetd_client.rs:31` — 6 candidate lines.
- `crates/session/greeter/src/greeter_app.rs:72` — 25 candidate lines.
- `crates/session/greeter/src/lib.rs:10` — 1 candidate lines.
- `crates/session/greeter/src/local_users.rs:25` — 5 candidate lines.
- `crates/session/greeter/src/main.rs:7` — 2 candidate lines.
- `crates/session/greeter/src/sessions.rs:4` — 11 candidate lines.
- `crates/session/installer/Cargo.toml:26` — 3 candidate lines.
- `crates/session/installer/src/action_button.rs:124` — 1 candidate lines.
- `crates/session/installer/src/disk_row.rs:4` — 5 candidate lines.
- `crates/session/installer/src/disks.rs:11` — 72 candidate lines.
- `crates/session/installer/src/install.rs:6` — 61 candidate lines.
- `crates/session/installer/src/installer_app.rs:223` — 38 candidate lines.
- `crates/session/installer/src/installer_network.rs:139` — 2 candidate lines.
- `crates/session/installer/src/keyboard_view.rs:274` — 1 candidate lines.
- `crates/session/installer/src/layouts.rs:306` — 1 candidate lines.
- `crates/session/installer/src/lib.rs:23` — 1 candidate lines.
- `crates/session/installer/src/main.rs:7` — 2 candidate lines.
- `crates/session/installer/src/media_profile.rs:12` — 6 candidate lines.
- `crates/session/installer/src/power_readiness.rs:38` — 1 candidate lines.
- `crates/session/installer/src/status_log.rs:205` — 1 candidate lines.
- `crates/session/installer/src/step_indicator.rs:313` — 1 candidate lines.
- `crates/session/installer/src/timezones.rs:5` — 5 candidate lines.
- `crates/session/qos-ntfs-helper/README.md:13` — 9 candidate lines.
- `crates/session/qos-ntfs-helper/build.rs:3` — 5 candidate lines.
- `crates/session/qos-ntfs-helper/build_native.py:1` — 2 candidate lines.
- `crates/session/qos-ntfs-helper/native/ntfs_reader.c:14` — 2 candidate lines.
- `crates/session/qos-ntfs-helper/native/sandbox.c:9` — 9 candidate lines.
- `crates/session/qos-ntfs-helper/src/bin/image_test.rs:1` — 4 candidate lines.
- `crates/session/qos-ntfs-helper/src/broker.rs:8` — 6 candidate lines.
- `crates/session/qos-ntfs-helper/src/broker/ipc.rs:15` — 27 candidate lines.
- `crates/session/qos-ntfs-helper/src/broker/linux.rs:15` — 20 candidate lines.
- `crates/session/qos-ntfs-helper/src/broker/session.rs:42` — 2 candidate lines.
- `crates/session/qos-ntfs-helper/src/broker/topology.rs:23` — 1 candidate lines.
- `crates/session/qos-ntfs-helper/src/lib.rs:1` — 2 candidate lines.
- `crates/session/qos-ntfs-helper/src/limits.rs:37` — 23 candidate lines.
- `crates/session/qos-ntfs-helper/src/main.rs:1` — 2 candidate lines.
- `crates/session/qos-ntfs-helper/src/manifest.rs:474` — 1 candidate lines.
- `crates/session/qos-ntfs-helper/src/stream.rs:48` — 1 candidate lines.
- `crates/session/qos-ntfs-helper/stage_install.py:1` — 1 candidate lines.
- `crates/session/qos-ntfs-helper/tests/build_native_fixtures.py:1` — 3 candidate lines.
- `crates/session/qos-ntfs-helper/tests/fd_boundary.rs:1` — 7 candidate lines.
- `crates/session/qos-ntfs-helper/tests/native_coordinator.py:1` — 5 candidate lines.
- `crates/session/qos-ntfs-helper/tests/native_copy_coordinator.py:1` — 2 candidate lines.
- `crates/session/qos-ntfs-helper/tests/native_images.rs:1` — 3 candidate lines.
- `crates/session/qos-ntfs-helper/tests/native_session.rs:1` — 2 candidate lines.
- `crates/session/qos-ntfs-helper/tests/native_session_coordinator.py:1` — 33 candidate lines.
- `crates/session/qos-ntfs-helper/tests/support/linux_boundary.rs:7` — 29 candidate lines.
- `crates/session/qos-ntfs-helper/tests/test_stage_install.py:30` — 1 candidate lines.
- `crates/session/qos-ntfs-protocol/examples/read_session.rs:2` — 2 candidate lines.
- `crates/session/qos-ntfs-protocol/src/client.rs:4` — 10 candidate lines.
- `crates/session/qos-ntfs-protocol/src/lib.rs:37` — 7 candidate lines.
- `crates/session/qos-ntfs-protocol/src/socket.rs:1` — 30 candidate lines.
- `crates/session/tilo-power/src/main.rs:6` — 10 candidate lines.
- `crates/session/tilo-unlock-helper/Cargo.toml:16` — 1 candidate lines.
- `crates/session/tilo-unlock-helper/src/main.rs:15` — 5 candidate lines.
- `crates/session/tilo-unlock-helper/src/pam.rs:5` — 18 candidate lines.

Summary: 24 grouped findings; 220 candidate-bearing files. No source modifications or executions.
