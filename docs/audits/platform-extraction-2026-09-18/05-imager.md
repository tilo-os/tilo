# A05 — Imager platform extraction audit

## Scope and method

Scope: `apps/imager`, including runtime Rust, embedded tests, the independent profile tool, examples, catalogs, and packaging. The audit covers target guards, native dependencies, FFI, processes, host paths, devices, permissions, volume identity, packaging, and the representative seams of the large transaction backend. Application-framework internals are outside its scope. Source evidence alone does not establish a build failure, runtime defect, or vulnerability.

Classification: **extract** = host detail in app/domain layer; **backend** = deliberate native implementation to relocate intact behind a capability boundary; **deployment** = retain target-specific build/release code; **test** = preserve platform-conditioned validation; **portable** = no native extraction indicated. Proposed interfaces are contracts, not claims about existing platform-framework APIs.

## Destination-design alignment

Read `docs/architecture/platform-services.md`. It is a proposed design, not evidence that APIs already exist. Expose the contracts below through `ui::platform`; implement non-rendering services in narrow `crates/system` or `crates/runtime` crates so headless profile tools do not pull in rendering. Hardware/elevated operations must be optional. The Imager filesystem/recovery **dependency closure** must move coherently: wherever this report says retain portable journal/recovery/reconcile logic, that means retain its portable semantics and separation from native primitives, not necessarily its current app file location. Move invariant-enforcing types, recovery codecs and policy required by native safety into the service closure without framework-to-app dependencies; only setup presentation and installation workflow choices must stay app-owned. Preserve sealed/move-only evidence, not public constructors for raw handles. A facade forwarding into app-owned implementations does not meet the design. Packaging exceptions should be documented narrowly in the eventual source-boundary check.

## Findings

**Current-tree note:** The source anchors in A05-06 through A05-09 and their
appendix tables are historical 2026-09-18 locations. The current tree no
longer contains the cited `destination_security`, `internal_cache`,
`connection/endpoint`, `connection/transaction`, `migration_durability`,
`migration_source`, or `migration_store` modules under `apps/imager/src/setup/`,
and no one-to-one replacement was verified here. Retain the architectural
findings and contracts, but treat those path tokens as historical anchors
until a fresh Imager source audit assigns current locations.

**Source-anchor note:** Verified prefix repairs identify current moved paths where
available. Any `:line` suffix remains the original 2026-09-18 audit coordinate; it
is historical evidence and does not assert that the cited symbol is at that line
in the current tree.

### A05-01 — Device inventory, identity and destructive media authorization (high; backend + extract)

Evidence: `apps/imager/src/probe.rs:75` shells to Linux lsblk; `:106` reconciles Flatpak inventory with host UDisks2; `:237` uses macOS diskutil/plutil; `:312` queries Windows disks. `apps/imager/src/write.rs:72` already defines a backend selector; `:265` implements Unix flock-based device exclusion, `:337` Windows exclusion, `:1013` UDisks object naming, `:1082` authorized descriptor opening, `:1188` helper mode, `:1463` Linux elevation, `:1489` macOS elevation, `:1520` Windows elevation, `:1588`/`:1626`/`:1637` native prepare and `:1660`/`:1678` native finish. These are intentionally privileged/platform operations, not accidental Linux-only code. Flatpak and host helper are distinct security authorities and must stay distinct.

Leaks: `apps/imager/src/model.rs:77`/`:86` captures source identity via inode/device versus Win32 file ID; `:347` constructs raw device paths in the app model; `apps/imager/src/app.rs:880` accepts a `/dev/` confirmation spelling; `apps/imager/src/write.rs:151` also embeds confirmation aliases. The UI and execution confirmation logic should consume the same device description/confirmation contract.

Impact: a port has to modify app model, UI, probing and elevated worker code together; collapsing these into generic `open(path)` could lose target identity and authorization protections. Suggested contract: read-only `MediaInventory` returns opaque device identity, display names, accepted confirmation tokens, capacity, system/removable safety evidence; `MediaWriter` acquires an exclusive authorized session, revalidates identity, prepares and yields owned I/O plus finalize/cancel semantics. Keep image copying/hashing/profile codec and product policy in app/shared portable code. Move OS path construction, locks, probes, elevation and native finalization together. Reuse `bounded-command` where currently employed; do not replace UDisks with sandbox escape.

Acceptance: native inventory parser fixtures on all hosts; root/system and changed-device refusal; matching UI/helper confirmations; no write in shots; Flatpak grants only reviewed descriptor; helper cancellation/progress/cleanup and temporary-file copy/readback tests retain their production paths. Privileged physical-media checks belong to an explicitly provisioned integration environment.

### A05-02 — Host architecture and build target are conflated outside Windows (medium; extract)

`apps/imager/src/host.rs:5` uses IsWow64Process2 for native Windows architecture, while `:41` returns `std::env::consts::ARCH` on all other platforms despite the module's native-host promise. This is evidence of a binary/native-host distinction left unimplemented outside Windows, not proof of a reproduced wrong download. `apps/imager/src/setup/desktop_probe.rs:100` also maps Rust architecture into release catalog names.

Move native architecture discovery into a `HostIdentity` capability reporting process architecture and physical/native architecture separately. Keep installer target profile selection and catalog naming in domain code. Add macOS translated-process/native-host evidence and preserve unknown-architecture refusal. Acceptance: injected Windows emulation and macOS translation cases select documented target architecture; no fallback silently converts unknown into x86_64.

### A05-03 — Native pickers and known folders live inside the app (medium; backend)

`apps/imager/src/probe.rs:24` adapts native dialogs to `ui::PortalChannel`, with AppleScript at `:39` and PowerShell/WinForms at `:55`. `apps/imager/src/setup/folders.rs:233`/`:248` implements parallel folder dialogs. `apps/imager/src/setup/known_folders.rs:93` reads XDG user-dir configuration and invokes xdg-user-dir; `:136` uses Windows Shell COM through PowerShell; other hosts explicitly return unavailable. `apps/imager/src/app.rs:4418` selects Linux portal versus this app-owned picker.

Extract picker implementations behind file/folder selection returning cancellation/failure and an authorized selection/grant (preserve portal semantics). Put known-folder discovery behind a separate suggestion-only service; suggestions must not become access grants. Keep labels, image filters and setup actions in the app. Reuse existing `PortalOutcome` shape at the caller without auditing framework internals. Native user-interactive dialogs have intentionally user-dependent duration; do not blindly apply a three-second probe timeout. Bound noninteractive folder discovery processes independently.

Acceptance: native Unicode/whitespace paths round-trip, cancel differs from error, missing native helper is surfaced, XDG folders resolving to HOME remain refused, no selection before explicit action. The non-Linux image-picker wrapper calls `native_image_path`, which has only macOS/Windows implementations; unsupported-target compile coverage needs an explicit fallback if additional targets are desired (source inference, not tested failure).

### A05-04 — Local storage roots and protected directory policy leak into view/flow (medium; extract)

`apps/imager/src/release.rs:439` chooses XDG_CACHE_HOME, then LOCALAPPDATA, then HOME/.cache regardless of current OS, then temp. This bypasses native macOS cache conventions and permits an unrelated OS environment variable to win. Other app modules instead use `dirs`: `setup/desktop_flow.rs:515`, `setup/connection/store.rs:558`, `setup/migration_store.rs:422`, `setup/internal_download_flow.rs:670`, `setup/import_view.rs:156` and `:675`, native Windows cache `setup/internal_cache/windows.rs:854`.

`apps/imager/src/setup/view.rs:1562` puts system-folder policy in a UI module: mixed Linux/macOS roots, special `/var/home` exception, and Windows first-component names. `:145` constructs host identity and `:187` branches native download setup by target. `setup/model.rs:35` carries product capability policy; retain that policy but feed discovered capabilities rather than distributing cfg logic across the view. `src/main.rs:120` explicitly disables runtime attachment on Windows: app-side wiring should consume capability availability, while app framework internals remain out of scope.

Provide `AppStorageLocations` (cache/private durable data, with explicit unavailable), `FolderEligibility` (native protected/system-root evidence) and setup capability discovery. Migrate root lookup first with compatibility/resume handling for existing stores; then replace UI OS cfg/heuristics with results. Do not move product labels or setup experience rules into a general platform crate.

Acceptance: platform-specific environment precedence, unset/relative env paths, existing cache migration, localized/redirected Windows system roots and macOS/Linux protected roots; no journal abandonment or temp fallback presented as durable storage; absent capability stays unavailable.

### A05-05 — Desktop installation is a deliberate Linux service backend (high; backend)

`apps/imager/src/setup/desktop_probe.rs:405` contains native Linux probing: `:556` package manager binaries, `:593` immutable-host markers/statvfs, process confinement and namespace/mount evidence, session support. `setup/desktop_install.rs:219` pins package input with Linux O_PATH and `/proc/self/fd`; `:349` holds Linux installation implementation and `:846` helper entry. `setup/desktop_worker.rs:14` refuses non-Linux and `:47` invokes `/usr/bin/pkexec`. `setup/desktop_continuation.rs:452` resolves XDG runtime readiness endpoint; `:478` checks owner/mode, `:528` verifies SO_PEERCRED and `/proc/<pid>/exe`, `:594` creates bounded nonblocking Unix connection. `setup/desktop_download.rs:74` has a Unix private package cache implementation.

Extract native probe/install/readiness into a `DesktopSessionInstaller` capability; retain `DesktopHost` facts, release compatibility decisions, flow state/events and review in the app. Keep dnf/apt/pacman target-specific adapters and Linux-only availability. Reuse current request/event and readiness evidence structures. Runtime peer attestation must be part of the endpoint capability, not a generic socket dialer that returns success for any listener.

Acceptance: unsupported/immutable/confined hosts refuse, reviewed package pinned and revalidated after elevation, bounded helper protocol, matching installed package/session evidence, no automatic session switch before valid readiness, hostile socket owner/peer/executable tests remain effective. Do not expand availability merely because extraction makes code compile elsewhere.

### A05-06 — Private filesystem ownership, durable receipts and native names (high; backend + mixed seams)

`apps/imager/src/setup/destination_security/mod.rs:11` dispatches Unix/Windows security. `unix.rs:18` obtains effective UID and `:83` checks filesystem semantics; `macos_acl.rs:14` binds Darwin ACL FFI; `windows.rs:12` uses tokens, DACLs, handle/volume attributes. These are security backends, not candidates for replacing with a portable chmod approximation.

`setup/migration_durability.rs:13` explicitly supports Linux/macOS and refuses others; `:32` uses Darwin volume UUID, `:68` Linux fs identity (with boot-bound fallback), later native persistence. `setup/nofollow.rs:6` and `:39` provide existing handle-relative no-follow/identity seams. `setup/migration.rs` owns copy orchestration and pinned directories; `setup/migration_source.rs:26` already has sealed `CopyFileSource`; `setup/migration_review.rs:26` captures metadata evidence. `setup/migration_store.rs:42`, `setup/migration_journal.rs:142`, and `setup/connection/store/format.rs:48` duplicate raw Unix-byte/Windows-UTF16 conversion. Preserve these encodings in persisted data; lossy Unicode is not an acceptable abstraction.

Move ownership/ACL validation, no-follow handles, stable identity, native name conversion, atomic publication and durable-directory guarantees behind a sealed `PrivateFilesystem`/`PinnedDirectory` capability family. Keep copy plan, journal meaning and replay policy in portable/domain services. Reuse `cap-std`, `cap-primitives`, `cap-fs-ext`, existing sealed source and security routines instead of new pathname-based access.

Acceptance: symlink/reparse/placeholder/hardlink and owner/ACL refusal, non-UTF8/unpaired-native-name handling, volume/boot identity across resume, directory-entry durability and crash recovery; Windows copying must remain unavailable until native durability is actually proven. Schema/golden-log compatibility is required.

### A05-07 — Verified persistent download cache and bundle handle operations (high; backend)

`apps/imager/src/setup/internal_cache.rs:8` dispatches Linux and `:11` Windows; `internal_cache/linux.rs:40` contains descriptor-relative opens, xattrs/ACL/mode and fs checks; `internal_cache/windows.rs:137` contains Win32 handle/reparse/stream/ACL semantics. The Linux file explicitly documents cooperative same-UID limitations: extraction must not advertise stronger hostile-mutation protection. `setup/internal_bundle.rs:359` dispatches OS component verification; `:423` Unix native handle traversal, `:636` Windows native verification. Bundle EFI path, command-line and manifest rules earlier in this file are target artifact format, not host APIs.

Extract only native held-cache/provider and component-open/identity primitives. Reuse existing `internal_cache` Provider/FreshProvider, checkpoint and coordinator separation; retain network transport/range/hash and bundle schema in `internal_download.rs`, and UI job lifecycle in `internal_download_flow.rs`. Preserve full file-handle ownership through verification/publication; no convenient conversion back to caller-supplied pathname.

Acceptance: existing Linux and Windows cache race/reparse, identity, cancellation, collision, disk-full and restart tests run on their own hosts; payload verification continues through held handles; checkpoints survive extraction unchanged; unsupported platforms remain refused; tests that require real filesystem semantics fail when unobservable rather than report a pass.

### A05-08 — Connected-folder endpoint evidence and NTFS source bridge (high; backend)

`apps/imager/src/setup/connection/endpoint.rs:23` selects native evidence; `:85` defines `ReadEndpoint`, `:194` `EndpointContext`, `:326` `PinnedEndpoint`. Linux `endpoint/linux.rs:14` uses ioctl, with later statx/export handles/sysfs/mount/block-device identity and clone/health checks. `endpoint/windows.rs:11` uses volume GUID/file IDs, DeviceIoControl, enumeration, streams and reparse/health evidence. `endpoint/unsupported.rs:1` explicitly refuses. `endpoint/linux/ntfs.rs:1` is a read-only administrative helper adapter using `qos_ntfs_protocol`, not an NTFS parser in this app. `setup/migration_native.rs:25` has an existing sealed transport, `:768`/`:826` Linux cancellation signal handling; `setup/migration_native_journal.rs:1` persists its copy semantics.

Move endpoint attestation/read session and native helper integration to platform services, retaining `EndpointIdentity`, capabilities and refusal detail at the boundary. Keep reconciliation and migration intent domain-side. Inspect adjacent `crates/session/qos-ntfs-protocol` and helper implementation in the responsible audit scope before changing protocol. A mounted path or filesystem label must never replace volume/root/health evidence; Windows NTFS semantics cannot be mapped to Unix strings.

Acceptance: clone ambiguity, mount swaps, removable/offline volumes, reparse/stream/placeholder refusal, stale helper session, malformed native names, health capability refusal, and helper cancellation remain tested. All unsupported capability combinations continue to fail closed.

### A05-09 — Transaction witnesses, leases and durable connection storage (high; backend)

`apps/imager/src/setup/connection/transaction.rs:549` exports only the Linux executor. `transaction/native/mod.rs:15` onward wraps lifetime-bound proof/session types; `transaction/native/linux.rs:8345` onward includes native nonce, descriptor metadata/openat/renameat2/fsync implementation amid a large transaction and recovery engine. `native/linux/witness.rs:54` uses inotify and `:100` validates procfs descriptor references. `native/linux/lease_process.rs:49` uses credentialed descriptor transport, native file leases/signalfd/pidfd and an isolated child; `:481` validates peer credentials; `:502` sets no-new-privileges. `setup/connection/mod.rs:13` and `src/main.rs:65` expose the early lease-receiver entry.

Durable storage has another intentional seam: `setup/connection/store/storage.rs:71`, `:942` Linux renameat2/macOS renameatx_np; `store/storage/windows.rs` delegates Windows durable handle operations via `connection/windows_native.rs:91`. `store/recovery.rs` and `store/format.rs` are mostly journal meaning/encoding, not native drivers.

Do not flatten this into a `Filesystem::rename()` facade. First move backend and its opaque proofs intact, retaining crate-private construction and lifetimes; then extract higher-level platform operations (validated transactional session, exclusive source read lease, native change witness, durable atomic private-store publication). Keep plan/reconcile/store policy and descriptor schema independently testable. The Linux witness state machine and Linux-only byte namespace types may stay backend-local even when they contain no FFI. Do not claim a Windows transactional executor exists because Windows read/store support exists.

Acceptance: native production-path race, inotify overflow/lost witness, helper death, signal isolation, lease break, crash/recovery and stale identity tests remain in Linux integration suites; Windows storage tests run separately; unavailable witness/lease is refusal, not optimistic fallback; no mutable public constructors for evidence.

### A05-10 — Build graph and standalone profile tool need a narrow shared codec seam (medium; extract)

`apps/imager/Cargo.toml:45` adds optional zbus; `:47` Unix libc; `:50`/`:53` selects Linux portal feature; `:56` owns a large Windows API feature list. Move native dependency/features to the platform implementation crate, retaining app-level `host-media-writer` and `flatpak-media-writer` as policy wiring where needed. Do not broaden qpkg permissions.

`apps/imager/profile-tool/src/main.rs:6` path-includes the entire production `model.rs`; `profile-tool/Cargo.toml:19` supplies serde/serde_json/sha2 and only Unix libc. The included `model.rs:89` references windows-sys under Windows but the standalone manifest has no Windows dependency. This is an unverified compile-risk inferred directly from the source; dead-code allowance does not erase typechecking. `apps/imager/src/profile_cli.rs:57`/`:62` embeds Unix no-follow flags and Windows reparse-open flags itself.

Extract a small portable install-profile model/codec shared normally by app and CLI, plus a narrowly scoped safe regular-file opening adapter for the CLI; avoid forcing GUI/platform UI dependencies into the release-media tool. Keep its isolated lockfile and edition 2024. Acceptance: standalone tool and app build/check on declared Linux/macOS/Windows targets, production encode/embed/readback tests share real codec, no symlink/reparse input bypass, dependency trees keep graphical stack out of profile tool. Do not regenerate either lockfile casually.

### A05-11 — Packaging and credentials are target deployment tooling (retain; deployment)

`apps/imager/packaging/package-host.sh:7` dispatches uname, packages Linux runtime libraries/AppImage and macOS application/DMG signing/notarization; `package-host.ps1:1` packages Windows runtime/installer/signature. `package-flatpak.sh:27` chooses the UDisks feature and `flatpak/org.tilo.Imager.yml:1` owns sandbox grants. OS launchers, Windows Inno Setup, Homebrew formula/cask and desktop/metainfo/plist are deliberately platform-specific deployment definitions.

Credential acquisition scripts implement trusted broker paths and private leases (`acquire-release-credentials.sh:5`, `Acquire-ReleaseCredentials.ps1:1`, `acquire-release-token.sh:1`). Signing, publisher and catalog scripts are release automation, not runtime credential APIs. Keep them in packaging (or a shared packaging toolkit if cross-project reuse is separately wanted), not a runtime platform crate. Keep architecture naming and feature flags aligned with extracted capabilities. Catalog Linux image/EFI targets describe the OS being installed, not assumptions about the machine running Imager.

Acceptance: packaging architecture, catalog and installer tests plus per-target installed smoke tests are retained; signing trust, broker permissions and sandbox configuration are unchanged. Never use test migration as a reason to run signing/publishing or inspect real credentials during an audit.

### A05-12 — Native tests must move with backend ownership, not disappear (high; test)

Dedicated tests include `setup/internal_cache/{tests.rs,linux/tests.rs,windows/tests.rs,coordinator/tests.rs}`, `setup/connection/{store/tests.rs,transaction/tests.rs}` and embedded tests in native endpoint/security/cache/transaction/storage files. Many rely on symlinks, real file IDs, ACLs, native child-process behavior and native filesystems. `setup/desktop_continuation.rs:654` uses HOME-backed temp storage; `internal_cache/linux/tests.rs:68` and `windows/tests.rs:77` use real user cache roots for appropriate filesystem semantics. `profile-tool/tests/embed.rs:1` and packaging test files are independent consumer tests.

Keep pure parsers/reconciliation tests portable; place native semantics in platform-specific suites and wire app integration tests through the same production capability entry. Existing `all(test, unix)` helpers enable testing portable policy on macOS without claiming Linux production support. Explicit runtime capability refusal is not a broken port. Acceptance: target matrix exercises each native backend, setup tests see equivalent refusal/availability, no ignored or weakened native assertions, no fixture bypass of production authority, test-specific temporary objects cleaned up. No tests were run as requested.

## Migration order

1. Freeze persisted schemas, capability/refusal behavior and security invariants; establish target test ownership.
2. Extract small host architecture, locations, picker and native-name seams; fix app consumers and standalone codec boundary.
3. Relocate existing secure filesystem/identity/cache backends with their sealed types and tests, preserving wire formats.
4. Relocate media and Linux desktop-session privileged backends, keeping authorization and target revalidation together.
5. Relocate endpoint, NTFS transport and transaction witness/lease backend as an intact unit before considering internal decomposition. Preserve helper startup dispatch and feature wiring.
6. Run narrow native/consumer tests and normal package checks on each supported host. Repository/spec-mandated finishing checks for subsequent implementation include `cargo check -p shell --tests --locked`; this audit did not build anything.

## Per-file inventory

Every file in the assigned scope appears below. An anchor at line 1 identifies a whole file where the classification applies; other anchors identify a representative actual native seam. Embedded tests are included in their owning runtime file. Portable entries are intentionally retained rather than swept into a platform framework merely for containing platform-named data.

### Runtime and service dependency closure

| File | Classification / finding | Responsibility |
|---|---|---|
| `apps/imager/src/app.rs:880` | extract; A05-01/03 | Device confirmation spellings and target-dependent picker dispatch inside presentation |
| `apps/imager/src/host.rs:5` | backend; A05-02 | Native Windows host CPU and non-Windows build architecture |
| `apps/imager/src/main.rs:65` | extract; A05-04/09 | Native helper startup dispatch, capability/build smoke wiring |
| `apps/imager/src/model.rs:77` | extract; A05-01/10 | Source identity and device-path construction; portable profile/target values and fixtures mixed in |
| `apps/imager/src/probe.rs:24` | backend; A05-01/03 | Pickers, disk inventory, UDisks safety evidence and source handle open |
| `apps/imager/src/profile_cli.rs:57` | extract; A05-10 | OS-specific safe file opening alongside portable CLI |
| `apps/imager/src/release.rs:439` | extract; A05-04 | Cross-OS cache environment policy; download/catalog algorithms portable |
| `apps/imager/src/setup/connection/endpoint/linux/ntfs.rs:1` | backend; A05-08 | Administrative read-only NTFS helper client |
| `apps/imager/src/setup/connection/endpoint/linux.rs:14` | backend; A05-08 | Linux ioctl/statx/sysfs/export/mount proof |
| `apps/imager/src/setup/connection/endpoint/unsupported.rs:1` | backend; A05-08 | Explicit unsupported refusal |
| `apps/imager/src/setup/connection/endpoint/windows.rs:11` | backend; A05-08 | Windows native volume/root/reparse/stream evidence |
| `apps/imager/src/setup/connection/endpoint.rs:23` | backend; A05-08 | Native selector and typed pinned/read endpoint authority |
| `apps/imager/src/setup/connection/mod.rs:13` | backend wiring; A05-09 | Early Linux lease receiver entry |
| `apps/imager/src/setup/connection/model.rs:20` | portable closure; A05-08 | Native encoding/name policy protocol types, not host API calls |
| `apps/imager/src/setup/connection/reconcile.rs:1` | portable closure; A05-08 | Deterministic reconciliation proposals, no native execution |
| `apps/imager/src/setup/connection/store/format.rs:48` | mixed closure; A05-06/09 | Native path conversion and portable bounded wire schema |
| `apps/imager/src/setup/connection/store/recovery.rs:1` | portable closure; A05-09 | Recovery meaning, move with invariant-enforcing store closure |
| `apps/imager/src/setup/connection/store/storage/windows.rs:1` | backend; A05-09 | Windows durable storage using native held operations |
| `apps/imager/src/setup/connection/store/storage.rs:942` | backend; A05-09 | Private storage; Linux/Darwin exclusive rename |
| `apps/imager/src/setup/connection/store.rs:558` | mixed closure; A05-04/09 | Private-data location and connection durable-store policy |
| `apps/imager/src/setup/connection/transaction/native/descriptor.rs:1` | portable closure; A05-09 | Private native transaction descriptor/recovery schema |
| `apps/imager/src/setup/connection/transaction/native/linux/lease_process.rs:49` | backend; A05-09 | Isolated credentialed file-lease child; signalfd/pidfd/SCM_RIGHTS |
| `apps/imager/src/setup/connection/transaction/native/linux/witness.rs:54` | backend; A05-09 | Inotify/procfs witness and change evidence |
| `apps/imager/src/setup/connection/transaction/native/linux.rs:8345` | backend; A05-09 | Linux transactional executor/recovery, descriptor syscalls, native fault tests |
| `apps/imager/src/setup/connection/transaction/native/mod.rs:15` | backend closure; A05-09 | Private lifetime-bound native proof/session wrappers |
| `apps/imager/src/setup/connection/transaction/native/witness.rs:20` | backend closure; A05-09 | Portable witness proof logic with Linux native namespace assumptions |
| `apps/imager/src/setup/connection/transaction.rs:549` | mixed closure; A05-09 | Portable transaction policy and Linux executor export |
| `apps/imager/src/setup/connection/windows_native.rs:91` | backend; A05-09 | Win32 security/stream/handle operation implementation and tests |
| `apps/imager/src/setup/desktop_continuation.rs:452` | backend; A05-05 | XDG authenticated readiness socket and executable attestation |
| `apps/imager/src/setup/desktop_download.rs:74` | backend; A05-05/07 | Unix private cache and verified package download |
| `apps/imager/src/setup/desktop_flow.rs:515` | extract; A05-04 | dirs cache selection; flow policy remains app-side |
| `apps/imager/src/setup/desktop_install.rs:219` | backend; A05-05 | Privileged verified package installation and native pinning |
| `apps/imager/src/setup/desktop_probe.rs:405` | backend; A05-05 | Linux host, package/session, confinement and immutability probe |
| `apps/imager/src/setup/desktop_worker.rs:47` | backend; A05-05 | pkexec/progress transport; shell fixture tests |
| `apps/imager/src/setup/destination_security/macos_acl.rs:14` | backend; A05-06 | Darwin ACL native FFI |
| `apps/imager/src/setup/destination_security/mod.rs:11` | backend; A05-06 | Security backend selection and shared refusal semantics |
| `apps/imager/src/setup/destination_security/unix.rs:18` | backend; A05-06 | UID, mode, native filesystem checks |
| `apps/imager/src/setup/destination_security/windows.rs:12` | backend; A05-06 | Windows token/DACL/volume/handle security |
| `apps/imager/src/setup/folders.rs:233` | backend; A05-03/06 | Native folder dialog plus no-follow scanner, native tests |
| `apps/imager/src/setup/import_view.rs:156` | extract; A05-04/12 | Data-root selection; Unix symlink demo/test fixtures |
| `apps/imager/src/setup/internal_bundle.rs:359` | mixed backend; A05-07 | Native held-component verification; portable target boot bundle validation |
| `apps/imager/src/setup/internal_cache/checkpoint.rs:1` | backend closure; A05-07 | Portable persisted checkpoint schema; preserve encoding |
| `apps/imager/src/setup/internal_cache/coordinator.rs:1` | backend closure; A05-07 | Native provider coordinator and cancellation state |
| `apps/imager/src/setup/internal_cache/linux.rs:40` | backend; A05-07 | Linux private cache descriptor/xattr/identity enforcement |
| `apps/imager/src/setup/internal_cache/windows.rs:137` | backend; A05-07 | Win32 private cache handles/ACL/streams |
| `apps/imager/src/setup/internal_cache.rs:8` | backend closure; A05-07 | Provider/native dispatch and held-read semantics |
| `apps/imager/src/setup/internal_download.rs:1` | portable consumer; A05-07 | Transport/range/hash operations over cache providers |
| `apps/imager/src/setup/internal_download_flow.rs:670` | extract; A05-04/07 | Data-root choice and cfg native provider selection; retain UI workflow |
| `apps/imager/src/setup/known_folders.rs:93` | backend; A05-03 | Linux XDG and Windows Shell folder discovery |
| `apps/imager/src/setup/migration.rs:1` | backend closure; A05-06 | Pinned copy engine and metadata handling; production-path adversarial tests |
| `apps/imager/src/setup/migration_durability.rs:13` | backend; A05-06 | Platform availability, volume identity and durable publication |
| `apps/imager/src/setup/migration_journal.rs:142` | mixed closure; A05-06 | Native path codec and snapshots; durable journal meaning |
| `apps/imager/src/setup/migration_journal_log.rs:1` | backend closure; A05-06 | Journal persistence via pinned native filesystem |
| `apps/imager/src/setup/migration_native.rs:25` | backend closure; A05-08 | Sealed NTFS transport/copy and Linux helper signals |
| `apps/imager/src/setup/migration_native_journal.rs:1` | backend closure; A05-08 | Native source journal identity and recovery receipts |
| `apps/imager/src/setup/migration_review.rs:26` | backend closure; A05-06 | Metadata/source identity review snapshot |
| `apps/imager/src/setup/migration_source.rs:26` | backend closure; A05-06 | Sealed read/attest/receipt source; Windows placeholder refusal |
| `apps/imager/src/setup/migration_store.rs:42` | mixed closure; A05-04/06 | Native path codec/private data root plus operation index |
| `apps/imager/src/setup/migration_worker.rs:18` | consumer/test; A05-06/12 | Durability capability adapter and native permission tests |
| `apps/imager/src/setup/mod.rs:16` | extract; A05-04/08 | Native feature availability module dispatch |
| `apps/imager/src/setup/model.rs:35` | portable; A05-04 | Product capability/host-target policy; retain app decisions |
| `apps/imager/src/setup/nofollow.rs:6` | backend; A05-06 | Shared descriptor-relative stat/open/identity seam |
| `apps/imager/src/setup/view.rs:145` | extract; A05-04 | Host capability cfg and protected-system-directory policy in UI |
| `apps/imager/src/write.rs:265` | backend; A05-01 | Native locks/elevation/raw-media preparation; preserve portable byte engine |

### Dedicated tests

| File | Classification / finding | Responsibility |
|---|---|---|
| `apps/imager/profile-tool/tests/embed.rs:1` | test; A05-12 | Preserve native/consumer tests with owning capability; inspect cfg before target execution |
| `apps/imager/src/setup/connection/store/tests.rs:1` | test; A05-12 | Preserve native/consumer tests with owning capability; inspect cfg before target execution |
| `apps/imager/src/setup/connection/transaction/tests.rs:1` | test; A05-12 | Preserve native/consumer tests with owning capability; inspect cfg before target execution |
| `apps/imager/src/setup/internal_cache/coordinator/tests.rs:1` | test; A05-12 | Preserve native/consumer tests with owning capability; inspect cfg before target execution |
| `apps/imager/src/setup/internal_cache/linux/tests.rs:1` | test; A05-12 | Preserve native/consumer tests with owning capability; inspect cfg before target execution |
| `apps/imager/src/setup/internal_cache/tests.rs:1` | test; A05-12 | Preserve native/consumer tests with owning capability; inspect cfg before target execution |
| `apps/imager/src/setup/internal_cache/windows/tests.rs:1` | test; A05-12 | Preserve native/consumer tests with owning capability; inspect cfg before target execution |
| `apps/imager/src/setup/internal_download/tests.rs:1` | test; A05-12 | Preserve native/consumer tests with owning capability; inspect cfg before target execution |

### Packaging and distribution

| File | Classification / finding | Responsibility |
|---|---|---|
| `apps/imager/packaging/Acquire-ReleaseCredentials.ps1:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/README.md:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/acquire-release-credentials.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/acquire-release-token.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/build-desktop-catalog.py:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/build-release-catalog.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/flatpak/org.tilo.Imager.yml:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/generate-winget.py:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/homebrew/tilo-imager-cask.rb.in:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/homebrew/tilo-imager.rb.in:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/linux/AppRun:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/linux/org.tilo.Imager.metainfo.xml:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/linux/tilo-imager:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/linux/tilo-imager.desktop:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/linux/tilo-imager.svg:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/macos/Info.plist:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/macos/tilo-imager:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/media-provenance.py:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/monitor-winget-acceptance.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/package-flatpak.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/package-host.ps1:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/package-host.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/package-install-medium.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/publish-forgejo-release.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/publish-homebrew-tap.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/publish-site-release.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/publish-winget.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/release-catalog.py:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/sign-installer-release.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/sign-linux-release-file.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/test-package-architecture.ps1:1` | test/deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/test-package-host.ps1:1` | test/deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/test_build_desktop_catalog.py:1` | test/deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/test_generate_winget.py:1` | test/deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/test_release_catalog.py:1` | test/deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/verify-appimage-runtime.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/verify-linux-release.sh:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/windows/Start-Tilo-Imager.ps1:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |
| `apps/imager/packaging/windows/tilo-imager.iss:1` | deployment; A05-11 | Target-specific packaging/release asset; retain deployment ownership |

### Profile tool, metadata and portable app files

| File | Classification / finding | Responsibility |
|---|---|---|
| `apps/imager/Cargo.toml:1` | build/consumer; A05-10 | Standalone codec dependency wiring or app target-native dependencies; preserve lock/schema |
| `apps/imager/desktop-catalog.json:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/examples/embed-profile.rs:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/profile-tool/Cargo.lock:1` | build/consumer; A05-10 | Standalone codec dependency wiring or app target-native dependencies; preserve lock/schema |
| `apps/imager/profile-tool/Cargo.toml:1` | build/consumer; A05-10 | Standalone codec dependency wiring or app target-native dependencies; preserve lock/schema |
| `apps/imager/profile-tool/README.md:1` | build/consumer; A05-10 | Standalone codec dependency wiring or app target-native dependencies; preserve lock/schema |
| `apps/imager/profile-tool/src/main.rs:1` | build/consumer; A05-10 | Standalone codec dependency wiring or app target-native dependencies; preserve lock/schema |
| `apps/imager/release-catalog.json:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/src/actions.rs:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/src/customize.rs:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/src/doc.rs:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/src/export_dialog.rs:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/src/iana_timezones.txt:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/src/lib.rs:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/src/profile.rs:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/src/setup/controller.rs:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/src/setup/desktop_catalog.rs:1` | portable; — | No native implementation identified; retain format/UI/domain data |
| `apps/imager/tilo-app.toml:1` | portable; — | No native implementation identified; retain format/UI/domain data |

## Summary and adjacent scope

12 grouped findings; all 129 scope files inventoried. Existing native backends dominate: preserve them as coherent security/recovery closures, while removing host assumptions from app models, views, paths and build wiring. Adjacent owners must inspect `crates/system/bounded-command`, `crates/session/qos-ntfs-protocol`, NTFS helper binaries and OS session/install packaging; those implementations were not audited here. Framework internals were intentionally excluded. No vendor subtree or build.rs was found in assigned scope. No actual native build/runtime/security verification was performed.
