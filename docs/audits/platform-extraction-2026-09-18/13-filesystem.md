# A13 — Filesystem platform extraction audit

## Scope and method

Scope: the `crates/system/fs/` manifest, daemon, library, modules, unit tests, integration targets, and fixtures. The audit covers target guards, FFI, Unix APIs, commands, paths, mount APIs, permissions, sockets, environment, persistence, and target dependencies. Application-framework and dependency implementations are outside its scope.

This is primarily a Linux filesystem service with deliberate FUSE deployment, already selecting `macos-no-mount` on macOS. Extraction should preserve the Linux backend and make resource/state services reusable without mounting. Destination alignment: `docs/architecture/platform-services.md` proposes a `ui::platform` facade over focused `crates/system` or `crates/runtime` services. Put the reusable host capabilities below that facade so this headless daemon does not acquire UI/rendering dependencies. Retain FUSE as a deliberate optional native backend, not as a universal hosted-app requirement. The design is proposed, and this report does not assume its contracts are already implemented. Source evidence establishes coupling and unguarded assumptions; no build or runtime failures were reproduced.

## Findings

**Source-anchor note:** Verified prefix repairs identify current moved paths where
available. Any `:line` suffix remains the original 2026-09-18 audit coordinate; it
is historical evidence and does not assert that the cited symbol is at that line
in the current tree.

### A13-01 — FUSE dependency and native mount lifecycle must become an explicit backend (high)

**Evidence:** `crates/system/fs/Cargo.toml:52` selects fuser for every non-macOS target, and `crates/system/fs/Cargo.toml:56` selects `macos-no-mount` on macOS. `crates/system/fs/src/lib.rs:26` exports the FUSE-containing module unconditionally. `crates/system/fs/src/fs.rs:30` imports fuser replies/types, `crates/system/fs/src/fs.rs:41` imports Unix filename bytes, and `crates/system/fs/src/fs.rs:4317` implements `Filesystem`. `crates/system/fs/src/main.rs:40` imports mount APIs; `crates/system/fs/src/main.rs:3005` constructs mount config; `crates/system/fs/src/main.rs:3014` chooses Owner versus All/AutoUnmount; `crates/system/fs/src/main.rs:3027` creates the real session, handles readiness and joins/unmounts it; `crates/system/fs/src/main.rs:3058` handles Ctrl-C. Linux-specific rename flags are correctly guarded at `crates/system/fs/src/fs.rs:4912`. Root `Cargo.toml:381` specifies fuser 0.17.0. The macOS no-mount behavior is intentionally tested at `crates/system/fs/src/lib.rs:179`.

**Platform/impact/classification:** intentional Linux/POSIX backend mixed with shared service construction. The macOS dependency feature disables native mounting but does not remove FUSE types or Unix assumptions from the library. The negative target selector is not a positive capability declaration for every other OS.

**Boundary/migration:** expose a platform `NativeFilesystemProjection` capability with mount options, readiness, cancellation, unmount/join and unsupported status. Move FUSE callbacks and errno/native-name conversion into its Linux backend; select the backend explicitly instead of interpreting not-macOS as supported. Keep daemon deployment and FUSE policy on Linux. Construct shared record services independently of native mount availability. Preserve owner-only harness configuration, read-only mounts, auto-unmount and capability ACL enforcement; do not substitute a permissive mount for unsupported platforms.

**Acceptance:** Linux real-kernel release suites still exercise the same production adapter; macOS service/core tests run without opening a native mount; unsupported native mount requests return explicit unavailable status before advertising readiness. Validate all intended target test compilations, not only library tests.

### A13-02 — Namespace and business operations expose FUSE/POSIX representations (high)

**Evidence:** `crates/system/fs/src/namespace.rs:23` imports `FileAttr`, `FileType`, `INodeNo`; `crates/system/fs/src/namespace.rs:110` maps logical `NodeKind` to fuser; `crates/system/fs/src/namespace.rs:1833` constructs native attributes, including mode, block geometry and `getuid/getgid` at 1856–1857; `crates/system/fs/src/namespace.rs:1865` publicly returns `FileAttr`. `crates/system/fs/src/fs.rs:2921` exposes access decisions as `Result<(), i32>`. Shared checked-history and preview implementation is attached to `QuickFS` (`crates/system/fs/src/checked_restore.rs:350`, `:386`, `:393`; `crates/system/fs/src/history_read.rs:84`, `:157`, `:169`), with POSIX/xattr translation at `crates/system/fs/src/checked_restore.rs:435` and `crates/system/fs/src/history_read.rs:252`. `crates/system/fs/src/main.rs:2640` uses fuser file type in recipient record projection.

**Platform/impact/classification:** leaked platform representations in otherwise portable CRDT/resource service. Hosted apps wanting history, access, metadata or hydration inherit a mount-oriented object and native errno vocabulary. Logical byte filenames are not themselves a portability bug: the CRDT must preserve raw names independently of the host filename encoding.

**Boundary/migration:** introduce logical `NodeMetadata`/node-kind, record/name identifiers and domain errors; reuse existing `collab_types::FileNodeKind`, `FileNameV1`, record/history request and outcome types. Translate UID/GID/mode/blocks/timestamps into fuser attributes only at the projection boundary. Extract the existing production history/access/restore methods into a shared filesystem service, retaining native xattr and errno adapters. Preserve hidden-versus-denied behavior so extraction cannot reveal ACL-filtered names.

**Acceptance:** identical ACL, checked-restore conflict/pending/unchanged outcomes and history paging through semantic calls and Linux FUSE; non-UTF8 names round-trip without lossy conversion; shared metadata carries no fuser type or native UID lookup.

### A13-03 — Multiple local protocols hard-code Unix sockets and Unix authorization (high)

**Evidence/all affected files:** `crates/system/fs/src/control.rs:21` / `:220` / `:285` / `:329`; `crates/system/fs/src/share_control.rs:25` / `:147` / `:406`; `crates/system/fs/src/recipient_control.rs:22` / `:116` / `:371` / `:407`; `crates/system/fs/src/storage.rs:40` / `:72` / `:86`; `crates/system/fs/src/workspace_authority.rs:445` / `:545` / `:579` / `:608` / `:655` (these functions are explicitly Unix-gated); `crates/system/fs/src/transport_gate.rs:5` / `:56` / `:67` (socket mode and peer UID). Binding/policy is additionally in `crates/system/fs/src/main.rs:864`, `:888`, `:2378`, `:2991`. Document socket chmod errors are discarded at `main.rs:868`, storage chmod errors at `main.rs:2994`; share-control chmod propagates at `main.rs:896`.

**Platform/impact/classification:** intentional local IPC backend leaked into framing and public client/server types. Unix imports remain unconditional in most modules. Security is not equivalent to replacing these sockets with localhost TCP: some trust owner or group socket permissions, and the optional transport gate checks the effective UID. The gate is release-harness instrumentation, disabled without its environment opt-in.

**Boundary/migration:** platform local endpoint with explicit owner-only or service-group policy and authenticated peer identity where required; generic framed streams separate from protocol handlers. Reuse current `runtime_fs::storage` messages and collab request/response types. Platform backend owns secure bind, stale-endpoint checks, permissions, peer checks and cleanup. Make permission failure fail closed rather than ignoring it while extracting. Preserve maximum frame/request/concurrency bounds and ACL enforcement. Keep network Iroh protocols separate from local trust semantics.

**Acceptance:** unauthorized local user/group cannot connect or issue accepted requests; failed endpoint security setup never starts a privileged service; existing framing tests run over an in-memory stream and native backend tests validate actual permissions/credentials. Transport partition gate stays explicitly opt-in with the same owner authorization.

### A13-04 — Deployment paths, system identity selection and systemd activation belong at host composition (medium)

**Evidence:** `crates/system/fs/src/workspace_authority.rs:29` and `:37` default to `/run/tilo` with `TILO_AUTHORITY_SOCKET_DIR`; their documented trust model is root/tilo-identity. `crates/system/fs/src/identity.rs:80` uses `IdentityPaths::default_system()` unless `TILO_IDENTITY_ROOT` overrides it. `crates/system/fs/src/main.rs:420` / `:423` default replica paths to `/var/lib/tilo/full-replica`; `crates/system/fs/src/main.rs:964` identifies managed service by `INVOCATION_ID`; `crates/system/fs/src/main.rs:2322` interprets systemd `LISTEN_PID/LISTEN_FDS`, takes FD 3, validates the socket pathname, sets CLOEXEC and clears activation variables. Installed service/unit assertions begin `crates/system/fs/src/main.rs:3159`, `:3182`, `:3241`, `:3283` and intentionally inspect `os/` files.

**Platform/impact/classification:** intentional Fedora/systemd deployment defaults and activation adapter, not generic filesystem rules. Hosted execution should not require root-owned service storage or infer its lifecycle from a systemd environment variable.

**Boundary/migration:** inject service paths and the existing identity provider/path object from host composition. Preserve `tilo-user-identity` signing/certification logic rather than creating new identities per mount. Put systemd descriptor activation and service restart semantics into a Linux service-host adapter with managed-service status and activated-listener capability. Keep exact installed Linux paths/unit expectations in OS deployment tests.

**Acceptance:** Fedora image retains its existing paths, service-group boundary and FD inheritance protections; hosted profile uses its own authorized storage/identity roots without systemd env; activation rejects mismatched descriptor count/path and children do not inherit the service endpoint.

### A13-05 — Predictive hydration probes Linux network/power and POSIX storage directly (high)

**Evidence:** `crates/system/fs/src/smart_monitor.rs:51` invokes bounded `nmcli` connectivity/device probes, `:93` calls `tilo_power_mgmt::sysfs_battery_snapshot` and `sysfs_on_battery`, `:112` uses Unix path bytes and libc statvfs. Numeric layout differences are cfg-adjusted at `:125`; the module itself and Unix import at `:7` are unconditional. `crates/system/fs/Cargo.toml:28` selects power with default features disabled. `crates/system/fs/src/smart_monitor.rs:147` starts repeated policy sampling; `crates/system/fs/src/smart.rs` holds the prediction policy/heuristics.

**Platform/impact/classification:** Linux backend assumptions in otherwise reusable predictive policy. Missing nmcli conservatively disables speculation; it is not proof of an application failure. Existing command limits/timeouts are useful safeguards. Sysfs availability semantics belong to the power backend and need adjacent review.

**Boundary/migration:** inject network connectivity/metering, power state (including unknown), and free-space observations from platform capabilities. Retain thresholds, queue limits, cancellation and foreground hydration policy in fs. Reuse power service abstraction where viable; do not create another nmcli implementation in each consumer.

**Acceptance:** unknown/unreadable/metered/low-power/low-space observations disable and cancel speculation while foreground reads remain functional; real native backend observations tested separately from deterministic policy tests; preserve bounded probes for Linux backend.

### A13-06 — Private storage, file identity and durable directory flush need shared platform semantics (high)

**Evidence:** `crates/system/fs/src/caps_file.rs:38` unconditionally uses Unix inode metadata to detect replacements. `crates/system/fs/src/clock.rs:85`/`:90` retain Unix file identity, `:142` and `:167` check owner/mode; these are guarded and relate to the explicit test-clock override rather than ordinary epoch arithmetic. `crates/system/fs/src/record_subscription.rs:347` writes mode 0600, `:406` validates permissions, and `:417` accepts every non-Unix permission state. `crates/system/fs/src/named_share_delivery.rs:390` / `:804` write mode 0600; `:840` validates it but `:848` has a non-Unix no-op. `crates/system/fs/src/recipient_mount.rs:384` creates a private directory and only chmods it under Unix at `:389`; `crates/system/fs/src/main.rs:2361` similarly prepares a private service parent. `crates/system/fs/src/record_transaction.rs:432` opens and syncs a directory but ignores failures.

**Platform/impact/classification:** security/durability abstractions are incomplete outside Unix; native checks should move, while persistence policy stays in shared fs. Non-Unix no-op branches are latent false assurance, not evidence this crate currently builds on Windows. Replacing inode checks with length/mtime alone would lose same-size/same-time replacement detection. Plain `std::fs` read/write is not automatically platform-specific.

**Boundary/migration:** extend/reuse `atomic-file` with explicit private file/directory policy, stable file identity, and durable directory update contracts; implement ACL equivalents or fail unsupported rather than unconditional success. Clock override requires owner-only, non-symlink and replacement-resistant access. Keep subscription/inbox state private and atomic. Specify whether journal removal durability is required and return failures accordingly rather than carrying best-effort directory syncing into every backend.

**Acceptance:** permissive secret files/directories fail on every supported host; atomic replacement and same-size/mtime replacement are detected; symlinks/owner substitution rejected according to current clock contract; crash/restart transaction tests prove backend persistence guarantees. Coordinate implementation with atomic-file/identity owners.

### A13-07 — Recipient mounting is coupled to spawning the filesystem CLI (medium)

**Evidence:** `crates/system/fs/src/recipient_mount.rs:205` spawns the current fs executable with mount-share/mount-named-share arguments; `:331` consumes stdout readiness; `:354` sends bearer payload via private stdin; `:364` kills child, with teardown supervision elsewhere in the module. `crates/system/fs/src/main.rs:2275` defaults to current executable. Private-path validation is covered by A13-06.

**Platform/impact/classification:** intentional process isolation/native projection mechanism, not generic subscription semantics. Hosted services cannot treat the current process executable as a CLI that supports native mounting. Preserve isolation between bearer authorities rather than merging all recipients into a wider-authority mount.

**Boundary/migration:** `RecipientProjectionHost` opens/stops one isolated projection for a subscription, reports readiness and supplies a resource handle or native mount path. Keep Linux subprocess backend and bounded private-stdin readiness protocol; report unavailable where no projection is selected. Keep subscription lifecycle in shared service.

**Acceptance:** exactly one projection per key, duplicate starts/reuse remain correct, no bearer material in argv/logs, failed startup reaps child and resources, revoke/stop tears down the correct recipient. Hosted semantic access must retain exact per-record authorization.

### A13-08 — Native integration tests need an explicit Linux test lane; current guards are insufficient (high)

**Evidence:** `crates/system/fs/tests/integration_test.rs:15` / `:16` import Unix filename/metadata APIs; `:31`–`:39` returns successfully when `/dev/fuse` is absent; `:968` / `:973` call the Linux four-argument getxattr ABI. This same target also contains useful no-mount ACL tests (`:335`) and dynamic grant tests (`:1169`). `crates/system/fs/tests/exact_share_process_fuse.rs:13` imports Unix APIs, `:236` / `:260` use Linux getxattr/setxattr signatures, `:374` requires `/dev/fuse`, `:379` requires fusermount3. `crates/system/fs/tests/collab_release_matrix.rs:8` / `:9` use Unix APIs, `:93` invokes fusermount3, `:348`–`:360` enforces real-FUSE availability, `:390` uses Linux getxattr. None of those targets starts with a Linux target gate. `crates/system/fs/tests/corruption_matrix.rs:146` correctly gates its permission helper; `crates/system/fs/tests/performance_gate.rs` yielded no native candidate. Unix shell-script unit fixtures occur in `crates/system/fs/src/recipient_mount.rs:434` / `:469` / `:504` / `:540` / `:597` and `crates/system/fs/src/recipient_control.rs:643` / `:683`. Socket unit tests reside in the A13-03 files; clock/identity/private-storage tests in A13-06 files.

**Platform/impact/classification:** intentional platform conformance tests mixed with portable tests; source-level ABI mismatch risk on macOS independent of no-mount feature. Existing silent FUSE skips contradict the repository's fail-if-unobservable covenant; preserve required release lanes, do not “fix” this by ignoring tests.

**Boundary/migration:** split shared semantic/in-process tests from Linux kernel targets; platform-select whole Linux ABI modules/targets, and maintain a mandatory Linux lane that fails missing prerequisites. Replace shell-script process fixtures with a portable purpose-built helper only when those supervisor contracts are cross-platform. Keep deployment tests separate and explicit. macOS no-mount assertion at `crates/system/fs/src/lib.rs:179` remains a useful backend-selection test.

**Acceptance:** all supported hosts compile all applicable test targets; portable ACL/history/persistence tests run without native mount; mandatory Linux kernel lane reports failure on unavailable FUSE and passes unchanged production semantics when provisioned. Do not reduce asserted invariants.

## Coverage appendix and portable logic to retain

All explicit native API/OS path/permission/process candidate files identified in scope appear above: Cargo.toml; src/{lib,main,fs,namespace,control,share_control,recipient_control,storage,workspace_authority,transport_gate,identity,smart_monitor,caps_file,clock,record_subscription,named_share_delivery,recipient_mount,record_transaction,history_read,checked_restore}.rs; src/checked_restore/tests.rs (errno expectation); tests/{integration_test,exact_share_process_fuse,collab_release_matrix,corruption_matrix}.rs. No fs-local build.rs exists; source thumbnail code uses the existing thumbnail service, and its manifest deliberately excludes native PDF/video decoding features.

Other enumerated files were searched and had no independent OS extraction finding: src/{acl,content,share_session,hydration,conflict_merge,thumbs,record_access,record_replication,docstore,availability,share_link,durable_backup,history,record_materializer,record_repository,authenticated_transport,eviction,share_management,record_publisher,store,syncstate,full_replica,record_mesh,transaction_coordinator,smart,record_protocol,replica_ack,conflict,sync,peer_auth,rich_metadata}.rs; src/record_transaction/legacy_restore.rs; src/peer_auth/tests.rs; src/tests/{mod,config_tests}.rs; tests/performance_gate.rs; src/tests/fixtures/sample.docx. This does not guarantee every path-dependent storage behavior is host-independent: direct reads/writes and dependency backends remain involved. `availability`, `durable_backup`, `full_replica`, record stores/journals and authority persistence already reuse atomic-file; keep their domain/state/recovery logic outside the platform layer. Iroh transport, cryptographic grants, CRDT merge, resource identity, Unix-epoch timestamps, logical absolute workspace paths and MIME/thumbnail interpretation are not OS-specific simply because a Linux daemon consumes them.

## Handoff

Eight grouped findings. Recommended sequence: logical metadata/error/service split (A13-02), backend/dependency selection (A13-01), shared secure IPC/storage capabilities (A13-03/A13-06), composition/lifecycle/prediction (A13-04/A13-05/A13-07), and explicit test lanes throughout (A13-08). Adjacent scopes: `crates/runtime/runtime-fs` xattr/client APIs, `crates/system/{atomic-file,bounded-command,power}`, identity and thumbnail crates, fuser dependency internals, and OS service/mount/security configuration. This audit identifies contracts to coordinate with those owners; it does not claim their implementation was audited here.
