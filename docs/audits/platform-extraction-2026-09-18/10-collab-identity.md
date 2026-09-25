# A10 — collaboration, identity, and hosting platform extraction

## Scope and method

Scope: source, tests, manifests, and documentation under `crates/collab/` and `crates/hosting/`. The audit covers target guards, native APIs and dependencies, processes, storage ownership and atomicity, path defaults, signals, sockets, environment variables, and platform-sensitive tests. Application-framework and adjacent system implementations are outside its scope. Findings are static source analysis rather than cross-target runtime qualification.

## Destination alignment

The proposed destination is `docs/architecture/platform-services.md` (read for this audit): focused non-rendering crates under `crates/system` or `crates/runtime`, optionally exposed to apps through `ui::platform`. All platform contracts proposed below should be implemented in those headless service crates; identity, relay, enrollment binaries and collaboration must not depend on the UI renderer. Reuse existing authorities (`atomic-file`, bounded-command, launcher, runtime identity and the external accounts secret-store boundary where applicable). Keep privileged handles/evidence private and capability-bound; moving code must not widen caller authority. The spec is proposed, so suggested contract names are not claims that an API already exists.

## Actionable findings

**Source-anchor note:** Verified prefix repairs identify current moved paths where
available. Any `:line` suffix remains the original 2026-09-18 audit coordinate; it
is historical evidence and does not assert that the cited symbol is at that line
in the current tree.

### A10-01 — secure identity persistence contains platform backends in the identity domain (high)

**Sites:** `crates/collab/tilo-user-identity/src/persist.rs:50` (process protection), `:65` (Linux secure open), `:110` (other platforms), `:340` (key checks), `:406` (owner policy), `:556` (exclusive key publication), `:585` (Linux renameat2), `:596` (Darwin renameatx_np), `:637` (temporary key), `:767` (mode validation); `crates/collab/tilo-user-identity/Cargo.toml:76` (`libc`).

**Evidence/platform:** Linux uses `PR_SET_DUMPABLE`, `openat2(RESOLVE_NO_SYMLINKS|RESOLVE_NO_MAGICLINKS)` and `renameat2(RENAME_NOREPLACE)`; Darwin has `O_NOFOLLOW_ANY` and `RENAME_EXCL`. Unix metadata checks enforce owner, permission mask and one link on the same open inode. Other Unix platforms get leaf-only `O_NOFOLLOW`; non-Unix drops ownership/mode/link checks and process protection. The no-clobber helper refers to Unix-only CString/OsStrExt outside cfg gates before its unsupported-platform branch. These are source-level portability/security-contract gaps, not tested build failures.

**Classification/impact:** deliberate security backend code misplaced in reusable identity logic; inconsistent non-Linux/Darwin guarantees. Extract without weakening all-component no-follow, bounded regular-file reads, same-inode metadata validation, one-link secrets or atomic no-replace behavior. Owner-only user keys and root-or-reader/group-readable machine keys are distinct policies.

**Boundary/migration:** platform `SecureFiles` operations taking semantic access/owner policy, durable replace versus exclusive publish, and `SecretProcessProtection`; keep key serialization/verification and zeroization here. Reuse/extend `atomic-file` for replacements, but its replace operation is expressly inappropriate for first-key no-clobber publication. Keep Linux/Darwin implementations in platform backends; unsupported security capabilities must return errors, not silently omit checks. Avoid exposing raw Unix mode values to identity callers.

**Acceptance:** race two creators and load the winner unchanged; reject wrong owner, extra hardlink, parent/leaf symlink, FIFO, oversized seed, and relaxed permissions; preserve 0600 user versus 0640 machine sharing; test crash durability and syscall failures on supported backends. Run backend tests on Linux and macOS; separately verify unsupported targets fail explicitly or compile only portable identity code.

### A10-02 — image trust and machine provisioning are explicitly Linux deployment backends (high boundary priority)

**Sites:** `crates/collab/tilo-user-identity/src/bin/tilo-image-trust.rs:3`, `:26`, `:90`, `:265`, `:574`, `:608`, `:626`, `:646`, `:669`, `:889`, `:898`, `:920`, `:972`; `crates/collab/tilo-user-identity/src/bin/tilo-device-key-init.rs:19`, `:24`, `:66`; manifest binary declarations at `crates/collab/tilo-user-identity/Cargo.toml:13` and `:17`.

**Evidence/platform:** image-trust unconditionally imports Unix APIs and uses Linux `SYS_openat2`, `prctl`, ownership checks and `flock`; parses bootc/composefs image status and uses `/run/tilo-verified-update.lock`. Device initialization requires euid 0, system identity paths and migration from `/var/lib/tilo/device/keypair.bin`, then directory fsync. Shared identity library and these installed binaries share one package.

**Classification/impact:** retain Bootc/early-boot orchestration as Linux deployment tooling, not a generic desktop service; isolate its compilation/dependencies from reusable cert/image verification types. No reason to port bootc operations to macOS.

**Boundary/migration:** reuse A10-01 secure I/O and a platform lock primitive for shared mechanics. Put Linux entry points behind explicit target/build boundaries or a dedicated Linux tools package while retaining pure image endorsement/cert validation here. Keep fixed lock paths and early-boot root policy owned by Linux deployment configuration.

**Acceptance:** portable identity consumers do not compile Linux-only binaries; Linux trust-chain tests still reject symlink substitution, wrong owner and rollback/concurrent-update races; legacy migration retains the same device key and refuses conflicting identities. Keep actual Linux integration coverage rather than skipping failed checks.

### A10-03 — requester lifetime and ownership are Linux procfs concepts exposed by the relay (high)

**Sites:** `crates/hosting/tilo-relay/src/process_lease.rs:17`, `:29`, `:60`, `:78`, `:94`, `:98`; exported unconditionally at `crates/hosting/tilo-relay/src/lib.rs:86`; enrollment requester arguments at `crates/hosting/tilo-relay/src/enroll.rs:89`; broker requester use at `crates/hosting/tilo-relay/src/home_peer_control.rs:503`.

**Evidence/platform:** process lease is PID plus `/proc/<pid>/stat` start ticks. Validation always reads procfs; owner validation is cfg(unix), although procfs is Linux-specific. This must not become a generic Unix implementation on Darwin. Identity/liveness is an authorization prerequisite during privileged publication.

**Classification/impact:** Linux backend leaking into transport/enrollment; unsupported platform would be unable to validate a requester with this implementation.

**Boundary/migration:** platform `RequestingProcessLease`/`validate_owner` with an opaque backend identity token; keep Linux PID/start-tick encoding only at its Linux helper boundary. Preserve repeated validation at publication checkpoints and anti-PID-reuse properties; return unsupported where equivalent validation is unavailable.

**Acceptance:** wrong user, dead requester, changed process lifetime and forged token all fail before publication; valid caller succeeds; macOS backend uses actual process identity facilities rather than `/proc`; Linux tests retain real process lifecycle checks.

### A10-04 — polkit user resolution, service control, helper execution and ownership restoration belong in a privileged platform broker (high)

**Sites:** `crates/hosting/tilo-relay/src/home_peer_control.rs:4`, `:14`, `:86`, `:153`, `:168`, `:180`, `:261`, `:340`, `:383`, `:390`, `:402`, `:433`, `:503`; `crates/hosting/tilo-relay/src/enroll.rs:304`, `:374`, `:789`, `:811`.

**Evidence/platform:** root check uses `/proc/self`; caller from `PKEXEC_UID`; home lookup manually parses `/etc/passwd`; identity root rules compare uid/mode and system path. Invokes systemctl, `/usr/bin/fs` and `/usr/bin/tilo-enroll`, manages `/etc/tilo`, and restores uid/gid using `chown` after atomic writes. Non-Unix ownership restoration currently returns success without doing anything. Broker already uses bounded-command and rollback service-state logic.

**Classification/impact:** intentional Linux privileged backend intermixed with enrollment/domain operations. `/etc/passwd` resolution is narrower than OS account databases even on Unix. A port cannot reproduce authorization merely by supplying a home directory and ignoring ownership.

**Boundary/migration:** typed privileged operations (`apply_host_configuration`, `approve_enrollment`, `pair_device`), trusted caller identity/home resolution, bounded fixed helper execution, service state snapshot/restore, and publishing artifacts with intended ownership. Preserve policy validation in the broker; retain `bounded-command` rather than introducing unbounded process spawning. Do not expose arbitrary command/service/path execution to clients. OS-native account resolution belongs behind the platform boundary.

**Acceptance:** authorization cannot be forged via caller-controlled paths/environment; outside-home and writable identity roots rejected; requester exit cancels publication; output remains user-owned after privileged write/recovery; service failure restores previous config and running state; non-Unix implementation does not succeed with omitted ownership work.

### A10-05 — filesystem locations and daemon lifecycle are embedded in domain defaults (medium)

**Sites:** `crates/collab/tilo-user-identity/src/persist.rs:149`; `crates/hosting/tilo-relay/src/config.rs:215`–`:228`; `crates/hosting/tilo-relay/src/admin.rs:15`, `:475`; `crates/hosting/tilo-relay/src/main.rs:38`, `:193`, `:229`; `crates/hosting/tilo-relay/src/enroll.rs:53`, `:59`, `:64`, `:70`, `:75`, `:95`, `:99`, `:101`, `:113`, `:2051`, `:2054`; `crates/hosting/tilo-relay/Cargo.toml:109`.

**Evidence/platform:** defaults spread `/var/lib/tilo`, `/var/lib/tilo-relay`, `/etc/tilo`, full-replica directories and `/var/cache/quick-fs`; admin separately resolves XDG_CONFIG_HOME then HOME/.config. main calls sd_notify Ready/Stopping and Unix SIGTERM/SIGINT, with ctrl-c fallback. These are lifecycle/path selection, not collaboration semantics.

**Classification/impact:** extract runtime policy; retain those exact defaults in Linux OS deployment. Existing configurable `IdentityPaths::under` and RelayConfig fields are useful injection seams. `sd_notify` is intentionally best-effort, not itself a defect.

**Boundary/migration:** platform locations differentiated by system identity, user identity/config, durable host state, cache and ephemeral status; service lifecycle adapter for readiness/stopping/shutdown. Inject resolved paths into config/CLI setup, not serialized wire protocols. Preserve explicit CLI/config overrides and machine-vs-user distinction.

**Acceptance:** clean temporary-root execution touches no real /etc or /var; native defaults resolve correctly on each host; override precedence remains deterministic; graceful shutdown drains existing relay behavior and platform readiness reflects actual readiness. Linux packaged services retain configured paths and notifications.

### A10-06 — compute has a good sandbox trait but still embeds Linux filesystem mechanics outside it (high)

**Sites:** `crates/hosting/tilo-relay/src/compute.rs:17`, `:265`, `:287`, `:316`, `:342`, `:355`, `:378`, `:587`, `:645`, `:706`, `:788`, `:903`, `:971`, `:985`; `crates/hosting/tilo-relay/src/compute_worker.rs:12`, `:67`, `:77`, `:111`; unconditional modules at `crates/hosting/tilo-relay/src/lib.rs:75` and `:78`; launcher/install/fs dependencies at `crates/hosting/tilo-relay/Cargo.toml:56`–`:61`.

**Evidence/platform:** LauncherComputeSandbox correctly rejects non-Linux launch and handles trusted DRM render nodes via launcher. However, shared executor imports Unix fd/modes, collects outputs from `/proc/self/fd/<directoryfd>`, applies 0400/0500/0600/0700 modes, uses O_NOFOLLOW and creates Linux sandbox request paths `/run/tilo/app/.tilo-compute`. FileComputeContentStore duplicates private-directory/no-follow operations. The launch cfg does not isolate those filesystem operations, including test fake launch paths.

**Classification/impact:** existing intentional Linux sandbox adapter plus OS assumptions leaked into otherwise reusable compute scheduler/executor. Preserve pinned-directory output collection against directory replacement; using a plain output path would weaken sandbox security.

**Boundary/migration:** reuse `ComputeSandbox`, `ComputeContentStore` and `ComputePackageCatalogue`; extract a capability-bound job directory/staging adapter that creates private immutable inputs, pinned output handles, bounded no-follow reads, executable snapshots and durable replay claims. Host sandbox paths belong to the backend invocation contract. Restrict backend dependencies/exports so relay transport clients can compile without local Linux worker implementation.

**Acceptance:** current replay rejection, payload integrity, output-byte bounds, cancellation and timeout checks remain; replaced output directory cannot expose staged inputs; symlink output rejected; CPU task receives no GPU node; unsupported hosts cannot advertise local execution; portable transport/domain tests do not need procfs.

### A10-07 — compute eligibility probes Linux power and launcher paths directly (medium)

**Sites:** `crates/hosting/tilo-relay/src/compute_worker.rs:555`, `:561`, `:607`, `:613`; advertisement at `:530`.

**Evidence/platform:** checks `/usr/bin/bwrap` or `/bin/bwrap`, `/run/tilo/energy-saver`, then `/sys/class/power_supply` type/capacity/online. Missing/unreadable power tree becomes `PowerAvailability::External`. That can misrepresent unavailable telemetry as external power; in current flow missing bubblewrap blocks readiness first, so this is not a claim that all non-Linux machines currently advertise eligibility.

**Classification/impact:** leaked platform availability/energy policy, affecting worker admission. Keep package integrity and job policy in hosting.

**Boundary/migration:** platform sandbox capability plus power/energy snapshot with explicit unknown/error state; backend interprets Linux sysfs/runtime flag or native OS APIs. Distinguish installed binary from sandbox actually available if the broader platform contract provides a probe.

**Acceptance:** battery, charging, low battery, energy saver, unknown telemetry and sandbox unavailable each produce deliberate admission results; no unintended external-power fallback on absent native telemetry; advertisements update when health changes.

### A10-08 — enrollment LAN discovery relies on platform socket delivery semantics (medium)

**Sites:** `crates/hosting/tilo-relay/src/enroll.rs:16`, `:994`, `:999`, `:1002`, `:1469`, `:1713`; `crates/hosting/tilo-relay/Cargo.toml:103`.

**Evidence/platform:** socket2 sets SO_REUSEADDR and Unix SO_REUSEPORT then wildcard-binds the well-known UDP port; senders broadcast IPv4 limited broadcast. Comment requires every active discovery/approval surface to receive the broadcast. Non-Unix omits reuse-port; portability depends on actual OS socket semantics/interfaces/firewall.

**Classification/impact:** platform networking detail in the enrollment CLI; socket2 is already an abstraction and this is not proof of a defect. Iroh transport elsewhere is a portable library boundary and need not be rewritten.

**Boundary/migration:** semantic local discovery transport supporting multiple subscribers, bounded receive/deadline and intended network scope; either native sockets or one platform-owned socket fan-out. Keep signing, encrypted offers, recipient matching and request limits in enrollment. No raw local-IPC peer-credential socket was found in this scope; ownership checks are broker/process/file based.

**Acceptance:** two simultaneous listeners receive intended discovery packets on each supported backend; unrelated packets and malformed offers rejected; deadlines work; no singleton bind conflict or silent packet partition; interface policy verified rather than inferred from Unix cfg.

### A10-09 — collaboration persistence repeats Unix private-file and durable-replace mechanics (medium)

**Sites:** `crates/collab/channel-crypto/src/persistence.rs:3`, `:40`, `:88`, `:133`, `:142`, `:153`; `crates/collab/channel-crypto/src/governance.rs:268`, `:279`, `:287`; `crates/hosting/tilo-relay/src/store.rs:643`, `:659`; existing atomic-file users also `crates/hosting/tilo-relay/src/admin.rs:296`, `:402`, `:469`, `:513`, `:541`, `crates/hosting/tilo-relay/src/daemon.rs:1250`, `:1285`, and enrollment writes at `:666`, `:671`, `:677`, `:699`, `:715`, `:759`, `:767`, `:773`, `:945`.

**Evidence/platform:** encrypted MLS snapshots and governance journal implement their own temporary-file/write/fsync/rename/directory-fsync with Unix-only mode enforcement. Snapshot chmods the parent 0700, including an already existing parent. On non-Unix private modes are omitted; directory opening/durability semantics need a native implementation. Relay Iroh secret loads through ordinary fs::read and creates via `atomic_file::write_with_mode(0600)`; private-read policy is less explicit than identity key loading.

**Classification/impact:** extract duplicated platform mechanics, not cryptography/serialization. Existing StateKey is supplied by a platform secret service, already the correct separation: do not add Secret Service DBus to channel-crypto. No native credential-store implementation exists here.

**Boundary/migration:** semantic private durable storage with explicit ownership of the parent directory; reuse/extend atomic-file rather than a third copy. Keep sealed-state format, nonces and AAD unchanged. Decide explicit secure-read policy for Iroh secrets using A10-01, without silently changing deployment ownership assumptions. Avoid blanket chmod on caller-owned shared directories.

**Acceptance:** ciphertext tamper/wrong-key rejection and governance atomicity unchanged; file/parent privacy guaranteed on each backend before visibility; old contents survive failed publication; rename/fsync failures propagate; secure-secret read and symlink policies covered; use of a shared parent cannot unexpectedly change unrelated directory access.

### A10-10 — native codec dependency needs an intentional media backend boundary (low/architecture)

**Sites:** `crates/collab/tilo-callstream/Cargo.toml:14`–`:17`; `crates/collab/tilo-callstream/src/encoder.rs:1`; `crates/collab/tilo-callstream/src/decoder.rs:1`; composition/transport in `src/session.rs`, `src/stream.rs`, `src/throttle.rs`.

**Evidence/platform:** OpenH264 source feature bundles native software codec code. No OS capture, hardware codec, camera/desktop device API or platform cfg was found in callstream. Native dependency is intentional, not evidence of Linux-only behavior.

**Classification/impact:** retain software backend, optionally move codec construction under platform media selection if platform framework owns media; leave framing/throttle/session protocol independent. Do not mass-refactor vendor codec or infer need for a new native backend.

**Migration/acceptance:** if extracting, a frame-in/encoded-packet-out codec contract preserving H.264 wire format and existing error handling; build codec on supported architectures, retain round-trip/quality tests, verify backend selection does not alter transport.

### A10-11 — native tests are not consistently isolated to their actual backend (medium)

**Sites:** `crates/hosting/tilo-relay/src/process_lease.rs:114` (unguarded Unix imports, procfs and /bin/sh); `crates/hosting/tilo-relay/src/compute.rs:1018`, `:1148` (Unix exit status, symlink and procfs-backed executor tests); `crates/hosting/tilo-relay/src/admin.rs:718` (unguarded Unix permissions); `crates/hosting/tilo-relay/src/enroll.rs:2590`, `:2638`, `:2720`, `:2766`, `:3067`; `crates/hosting/tilo-relay/src/home_peer_control.rs:667`; `crates/hosting/tilo-relay/tests/personal_device_process.rs:248`, `:351`, `:354`, `:361`, `:399`, `:408`, `:447`; `crates/collab/tilo-user-identity/src/persist.rs:796`, `:814`, `:837`, `:856`, `:872`, `:897`, `:913`, `:989`, `:1019`, `:1038`, `:1073`, `:1133`, `:1157`; image-trust tests `src/bin/tilo-image-trust.rs:1055`, `:1080`, `:1113`, `:1139`; device-init tests `src/bin/tilo-device-key-init.rs:74`.

**Evidence/platform:** security tests use Unix permissions/link operations while Linux integration test uses root, CAP_SYS_ADMIN, mount namespaces, unshare, /proc, /usr/bin/setpriv and a synthesized passwd entry. That integration test has a pre-existing `#[ignore]` attributed to explicit fs-fuse CI execution and cfg(unix) body despite being Linux-only. This audit did not run/verify that CI route.

**Classification/impact:** native backend contract tests; preserve them, but actual target selection must match Linux rather than all Unix. Pure domain tests should not acquire procfs dependencies from test fixtures.

**Migration/acceptance:** move native cases alongside extracted backend while keeping portable protocol/domain tests active on every supported host. Add proper platform test-target selection and explicit required Linux privileged CI execution; never weaken assertions or introduce skips/ignore to make tests green. Verify production-path tests fail if their subject cannot be observed. Audit existing ignored acceptance execution separately against the repository covenant.

## Portable concepts and false positives to retain

- `crates/collab/collab-types/src/files.rs:48`: byte-preserving `FileNameV1` describes Unix filenames, but its wire bytes deliberately preserve signed-record compatibility. Keep wire representation stable; native filesystem projection/name validation belongs in filesystem platform adapters. Do not convert to a host-specific string representation or resign old records.
- Protocol Unix timestamps, UUIDs, resource paths, CRDT operations, crypto entropy via rand and lexical `rename` actions are not OS extraction findings. `/tmp` strings in actions tests are sample resource identifiers.
- Scans of actions, collab-types (including files/history), reference, threshold-identity, collab-fixtures, identity cert/canonical/approval/enrollment/revocation/directory/image/projection, and relay auth/policy/coedit/antientropy/transport/client/peer/workspace/mesh/compute transport/session found no direct native backend that warrants moving those whole modules. Preserve their domain ownership.
- Standard fs persistence in store/config/home_peer and ordinary Iroh network operations are not individually reasons to extract whole modules. Prioritize actual semantic security/durability/path/lifecycle boundaries above.

## Recommended implementation order and adjacent owners

1. Secure file/owner/durable publication and process identity contracts first (A10-01/03/09), with Linux/Darwin behavior frozen by tests.
2. Isolate Linux privileged/deployment binaries and their lifecycle/configuration (A10-02/04/05); do not port bootc itself.
3. Extract compute staging and health through existing traits (A10-06/07), then discovery transport (A10-08).
4. Adjust native dependency/test target boundaries (A10-10/11), preserving wire formats and real security acceptance tests.

Coordinate with owners of `crates/system/atomic-file`, `crates/system/bounded-command`, `crates/packaging/{launcher,install,pkg}`, `crates/runtime/runtime-identity`, `crates/system/fs/`, shell enrollment callers and Linux OS service/polkit/CI definitions. Their internals are adjacent scope, not covered here. Use the proposed system/runtime implementation crates and ui::platform facade described above; proposed API names describe contracts, not existing symbols. Do not move headless mechanisms directly into ui or introduce a headless dependency on ui. Preserve Cargo.lock and authorization boundaries throughout extraction.

**Summary:** 11 grouped findings (including intentional deployment/native backends and test constraints), with extraction concentrated in identity filesystem security, privileged enrollment, relay lifecycle and compute. Most collaboration protocol/crypto/domain modules should remain where they are.
