# Private installer cache on macOS: native design

Date: 2026-09-18; corrected 2026-09-19 after [preflight review](private-cache-macos-preflight-review.md). **Design for parent review, not implementation or qualification evidence.** This is the next N02 slice after the accepted cache source extraction. Read [framework-contract.md](framework-contract.md), [source extraction design](private-cache-extraction-design.md), [native reconnaissance](private-cache-native-followthrough-recon.md), and the [implementation plan](../2026-09-18-private-cache-macos.md). Planning is already authorized. Product work is outside this design task; corrections touch these two planning documents and the SDD correction report. The parent owns source handoff, acceptance and implementation dispatch.

## Decision and scope

Implement one private macOS provider for the existing checkpoint/coordinator protocol. First extend **atomic-file**, the established framework owner, with held relative operations, security observations and owned namespace mutations that the provider actually needs. Reuse `FileLease` for the permanent root sentinel. Keep macOS admission policy, cache layout, freshness, journal interpretation and retained component state inside `tilo-install-artifacts`. No provider, filesystem selector, path, file handle or preparation receipt reaches `ui::install_artifacts`.

Three approaches were considered:

1. **Shared primitives followed by a macOS provider (chosen).** Adds the missing filesystem contracts once while retaining the existing cache protocol and narrow review boundaries.
2. Copy Linux and replace syscalls. Rejected: duplicates shared publication/ACL/lease mechanisms, mistakes Linux ACL and filesystem assumptions for Darwin behavior, and encourages unsafe cfg expansion.
3. Replace all Linux/Windows/cache/provisioning code with one generic storage engine. Deferred: it mixes Windows namespace completion and existing persisted identities with a new backend. This slice gives later consolidation concrete primitives and conformance evidence without claiming S01/S02 are complete globally.

The outcome is a functional production macOS cache on the admitted profile, with honest errors and uncertainty. It does not enable staging, installation or raw-media writes. The existing `open_legacy_imager` entrance remains transitional G02 debt; an application-global client, detached wake handle or invented grant token would not fix authenticated invocation or revocation.

## Inspected source and actual gaps

| Existing implementation | Reuse / required addition |
| --- | --- |
| `cache/checkpoint.rs::{Provider,FreshProvider,Session}` | Keep protocol, receipts, encoding, digest/checksum and reset ordering. `File::sync_all` already invokes Darwin `F_FULLFSYNC` in pinned Rust; no new provider method merely to rename this flush. |
| `cache/linux.rs` | Behavioral reference for bounds, retained ancestry/security, unknown-residue refusal and all reader postchecks; its syscall/ACL code is not a macOS implementation. |
| `cache/windows.rs` | Independent reference for held sharing/security and identity; `namespace_boundary` presently validates only. No Windows durability claim is imported. |
| `cache/coordinator.rs` | One existing worker/control loop; instantiate it with `macos` in `native_coordinator.rs`. First remove fixture construction/test inclusion from the shared source and own them solely in the fixture wrapper. Keep private consumption and cooperative cancellation/join behavior. |
| `cache/fixture_{platform,storage,coordinator}.rs` | Explicit protocol/transport fixtures only. Never choose these in the normal macOS constructor. |
| `StorageDirectory::{identity,open_read,validate_privacy}` | Reuse no-follow regular reads and identity. Add relative directory opens, bounded enumeration, nontruncating writable opens, exclusive creation and exact security observations. |
| `StorageDirectory::publish` | Reuse for bounded byte publications where its staging protocol is appropriate. **Do not** substitute it for direct exclusive `record-N` creation: changing journal failure residue would change recovery semantics. It cannot publish an already streamed partial without copying it. |
| `provision_directory` / `reconcile_directory` | Their trusted-host canonicalization, path re-observation and release of successful ancestor custody do not meet this cache's retained raw ancestry contract. Keep preference use unchanged; compose the new relative operations in a private cache ancestry vector instead. |
| `commit_existing` | Commits a matching existing name but opens it again for commit. Not the primitive for the streamed partial, nor a replacement for cache ownership/freshness. |
| `FileLease::{try_acquire,identity,metadata}` | Consume the sole sentinel handle and retain lease until every `Arc<LockedRoot>` is gone. Add security observation through that private handle; never duplicate it or export it. Existing `StorageDirectory::try_acquire_lease` creates with inherited policy, so do not use it for the private cache sentinel. |

## Shared primitive contracts

These are framework-internal public Rust APIs in atomic-file, **not SDK exports**. Native modules stay private. Implement the Unix operations on Linux and macOS; preserve other targets' existing API/builds. Avoid changing the existing `Privacy::OwnerOnly` policy for unrelated consumers.

### Observation and held child I/O

Add `security.rs` and a private `native/unix/macos.rs` module; move the current `macos_acl` implementation there without weakening its conservative policy. Proposed signatures: `SecurityPolicy`, `SecuritySnapshot`, `FileLease::security_snapshot`, `FilesystemInfo`/`FilesystemKind` and all new directory methods below are `#[cfg(unix)]`, with Linux and macOS implementations. `PersistentIdentity` and `StorageDirectory::persistent_identity` are `#[cfg(target_os = "macos")]`. No Apple ABI type enters a Linux signature or unconditional Windows export:

```rust
pub enum SecurityPolicy { OwnerOnly, OwnerControlledDirectory }
pub struct SecuritySnapshot { /* private, Clone + Debug + Eq + PartialEq */ }
impl SecuritySnapshot {
    pub fn of(file: &File, policy: SecurityPolicy) -> io::Result<Self>;
}
impl FileLease {
    pub fn security_snapshot(&self, policy: SecurityPolicy) -> io::Result<SecuritySnapshot>;
}
impl StorageDirectory {
    pub fn metadata(&self) -> io::Result<Metadata>;
    pub fn security_snapshot(&self, policy: SecurityPolicy) -> io::Result<SecuritySnapshot>;
    pub fn open_directory(&self, name: &OsStr) -> io::Result<Self>;
    pub fn open_read_write(&self, name: &OsStr, privacy: Privacy) -> io::Result<File>;
    pub fn create_file_new(&self, name: &OsStr, privacy: Privacy) -> Result<File, NamespaceError>;
    pub fn create_directory_new(&self, name: &OsStr, privacy: Privacy) -> Result<Self, NamespaceError>;
    pub fn entries_bounded(&self, max_entries: usize, max_name_bytes: usize) -> io::Result<Vec<OsString>>;
    pub fn filesystem_info(&self) -> io::Result<FilesystemInfo>;
}
```

`OwnerOnly` observes effective-owner UID, complete mode, GID, native flags and complete ACL after validating owner-only effective access and regular-file single-link count where applicable. `OwnerControlledDirectory` allows root or effective UID ownership, forbids group/other write and special mode bits, permits traversal/read, and rejects ACL allows to other principals conservatively. Both snapshots include ACL ordering, flags, qualifier UUIDs and rights, including deny/inherit-only entries; an otherwise safe ACL change must compare unequal. Snapshot equality excludes file length and times so ordinary download writes do not invalidate security. Cache separately requires exact `0700`/`0600`, rejects special bits and checks type/link count on every observation.

On macOS, bound ACL export before allocating using `acl_size` plus `acl_copy_ext`, maximum 4096 bytes; preserve the existing descriptor-based allow/deny validation. Distinguish absent ACL from observation failure; unsupported ACL retrieval is refusal, not empty. No `chmod`, ACL removal, owner repair or path-based ACL reads. Linux observation preserves the existing qualified POSIX mode-mask policy and bounded access/default ACL bytes. Existing generic privacy APIs retain their current policy and callers.

`FilesystemInfo` is an observation, not certification: private fields with `kind() -> FilesystemKind`, `is_local() -> bool`, `is_read_only() -> bool`, `enforces_ownership() -> bool`, `exclusive_rename() -> Option<bool>`, `volume_uuid() -> Option<[u8;16]>`. `FilesystemKind` contains `Apfs, Ext4, Xfs, Btrfs, Other`. Do not admit from kind alone. Darwin uses held `fstatfs` and bounded `fgetattrlist` volume queries, checking returned/valid attribute and capability masks; Linux maps its existing native observations. Unknown capability is `None`, never assumed true. Do not change existing `FileIdentity` equality or Linux/Windows persisted digest domains.

All names are single normal raw components. No-follow, close-on-exec, nonblocking admission precedes ordinary file use; reject FIFO/device/directory in file APIs without blocking. Exclusive creation is `0600` or `0700` from creation for OwnerOnly and validates inheritance before and after creation. It does not repair preexisting collisions or silently delete failed creations. Writable open neither creates nor truncates. Enumeration opens `.` relative to the held directory for an independent offset, skips only `.`/`..`, bounds names/count before collection, propagates `readdir` errors and sorts raw bytes. Cache calls `(16,255)`, then applies its stricter journal/root counts. Successful creation is **not** a durability acknowledgement; callers commit the new object and parent explicitly.

### Owned namespace mutation and commit

Add `namespace.rs`; its new types/exports and the methods below are `#[cfg(unix)]`, implemented on Linux and macOS. Use the existing `FileIdentity`, `PublicationState` and `PublicationError` where their meanings match, without conflating unlink and publication:

```rust
pub enum NamespaceDurability { Native, DeviceFlush }
pub enum NamespaceState { Unchanged, ChangedDurabilityUnknown, NamespaceUnknown }
pub struct NamespaceError { /* private source + state; no destructor cleanup */ }
impl NamespaceError {
    pub fn state(&self) -> NamespaceState;
    pub fn io_error(&self) -> &io::Error;
}
impl StorageDirectory {
    pub fn validate_commit_requirement(&self, requirement: NamespaceDurability) -> io::Result<()>;
    pub fn commit_namespace(&self, requirement: NamespaceDurability) -> io::Result<()>;
    pub fn rename_owned_if_absent(
        &self, source: &OsStr, file: &File, expected: &FileIdentity,
        target: &OsStr, privacy: Privacy, durability: NamespaceDurability,
    ) -> Result<(), PublicationError>;
    pub fn remove_owned(
        &self, name: &OsStr, file: &File, expected: &FileIdentity,
        privacy: Privacy, durability: NamespaceDurability,
    ) -> Result<(), NamespaceError>;
}
```

`rename_owned_if_absent` retains a directory clone and file clone for a retryable commit (ordinary payload clones are allowed; lease clones are not). It validates the held file and relative source's identity/privacy, syncs the supplied file, calls the native same-parent exclusive rename, verifies destination identity/privacy, and commits the namespace with the explicit requested durability. Admission of that requirement precedes submission. The retained retry guard records the requirement as well as identity/privacy; it cannot retry with a weaker policy. It never creates a new staging copy, overwrites, links/unlinks as fallback, or reopens an ambient path. Proven collision yields NotPublished/AlreadyExists and leaves both files. Any submitted ambiguous result or post-rename check failure retains both possible names and reports uncertainty. A known successful rename followed only by barrier failure uses a retained commit guard; retry revalidates the destination against the held file and repeats barriers only, never another rename. Extend the existing private commit-guard representation rather than adding a parallel public publication error.

`remove_owned` validates both the supplied handle and its relative name immediately before unlink; submitted errors retain uncertainty. On unlink success it commits the parent with the requested durability; barrier failure reports ChangedDurabilityUnknown. Drop never removes anything. This is a cooperative check-then-unlink contract, not atomic identity-conditioned deletion or protection from a hostile same-UID mutator. The cache rescans and validates journal content/security before invoking it and poisons itself before the first retirement effect.

`NamespaceError` applies to exclusive create as well: collision/preflight is Unchanged; a known creation followed by admission failure is ChangedDurabilityUnknown; ambiguous submitted native failure is NamespaceUnknown. Exact owned residue remains for recovery. Cache must not classify every pre-handle error as harmless.

`NamespaceDurability::Native` uses the existing shared native namespace contract; `DeviceFlush` requests the stronger explicit platform barrier sequence, with no downgrade. The **new cache always passes DeviceFlush** for created/adopted cache edges, journals, finals and retirement. `validate_commit_requirement` observes admitted filesystem/capability/mount constraints before effects; it does not promise a future syscall cannot fail. Native implementations belong only to atomic-file. On Linux the new DeviceFlush operation uses real directory fsync on ext4/XFS/Btrfs under its documented native contract; it does not expand the old cache profile or claim other profiles. Existing Linux generic permission admission, including tmpfs/OverlayFS, is unchanged. Unix APIs do not change Windows behavior.

The **candidate** Darwin DeviceFlush sequence is held-directory `fsync`, then held-directory `F_FULLFSYNC`, admitted initially for local ownership-enforced writable APFS. Task 0 must obtain and review native feasibility before Task 2 implements this new operation. File data keeps pinned `File::sync_all`. Existing `Directory::sync`, `StorageDirectory::publish`, provision/reconcile and existing-commit callers retain their previous behavior and admission; an APFS-only receipt never authorizes their unconditional conversion. Share the private fsync helper where appropriate, then add the stronger sequence behind the explicit new requirement. The new rename guard may be a private CommitGuard variant so existing publish retries also retain their prior semantics. No duplicated cache syscall, silent fallback or permanent unsupported macOS substitute is permitted. If the candidate fails, Task 0 revises the shared protocol with primary/native evidence before Task 2; syscall feasibility still does not prove crash ordering.

| Consumer/domain | Admission and directory types | Operation/evidence boundary |
| --- | --- | --- |
| New macOS cache | Local writable APFS, both case modes, ownership/ACL/exclusive-rename checks; newly created/adopted private and controlled writable edges | DeviceFlush required; direct Task 0 feasibility, Task 2 production reachability and Task 5 crash evidence are separate. |
| New cache existing system ancestry | Held root-owned/read-only APFS System/Data/firmlink traversal witnesses | Observe identity/security; do not create there or require a new DeviceFlush on a read-only witness. Task 0 records these directory outcomes separately rather than confusing them with mutable cache edges. |
| Existing macOS OwnerOnly consumers | Current conservative ACL/DAC admission, no APFS-only filesystem restriction; private files/directories and existing traversal ancestors | Existing publication/provision/reconcile/commit contract remains unchanged. APFS regression is required; unobserved HFS+/other admitted profiles are open evidence, not certified or newly excluded. |
| Existing macOS Inherit consumers | No added privacy filesystem restriction, including previously admitted network/FUSE/other mounts and read-only traversal parents | Preserve previous behavior; no blanket DeviceFlush rollout. Future stronger migration requires its own actual domain inventory and matching evidence. |
| Existing Linux consumers | Generic permission profiles include ext4/XFS/Btrfs/tmpfs/OverlayFS; old cache is stricter | Preserve existing calls/admission and old persisted identity domains. New operation conformance is scoped separately. |
| Existing Windows consumers | Existing namespace/publication/privacy/profile rules | No change or certification from this Unix slice. Windows namespace completion remains open. |

The exact Task 2 → Task 3 typed-phase handoff is mandatory:

| Observed boundary | Required classification and custody |
| --- | --- |
| Invalid component, failed preflight/security, or proven exclusive-create collision before an effect | NamespaceState::Unchanged; pre-submit rename failure is NotPublished. No repair or deletion. |
| Native create succeeded, then open/type/privacy/snapshot admission failed | ChangedDurabilityUnknown; retain the exact named residue even if no File was returned. |
| Mutation submitted with effect unestablished | NamespaceUnknown / PublicationUnknown; retain possible names and poison. An injected errno proves no native outcome. |
| Native exclusive rename returns EEXIST | NotPublished/AlreadyExists; preserve both names and authenticate an existing final separately. |
| Rename succeeded, destination identity/privacy validated, then barrier failed | PublishedDurabilityUnknown; retain held file/parent/name/identity/privacy/durability for commit-only retry. |
| Retry observes replacement | Refuse without rename/unlink; preserve replacement and retained uncertainty. |
| Unlink succeeded, then barrier failed | ChangedDurabilityUnknown; no destructor cleanup or clean-journal claim. |
| Observation after the final successful barrier | Observation-only unless explicitly simulating lost acknowledgment. A synthetic EEXIST after success is lost acknowledgment, never native collision. |

Observer errors cannot masquerade as syscall failures. The provider inspects these typed phases and sets residue/poison/uncertainty **before** converting the I/O source into Failure; diagnostic-string parsing is forbidden.

## macOS policy and location

Initial required profile: writable **local APFS**, ownership enforced (`MNT_IGNORE_OWNERSHIP` clear), actual ACL retrieval, valid exclusive-rename capability, working held directory barriers. Exercise both case-sensitive and default case-insensitive APFS. Cache-created names are canonical ASCII; enumerate and compare exact spelling, refuse case aliases/unknown residue, and authenticate selected component filenames. No HFS+, network, FUSE, FAT/exFAT, ownership-disabled or cloud-provider-backed cache certification follows from this slice. Those configurations receive explicit Unsupported/PermissionDenied as appropriate; support expansion requires a real profile, not a fallback. APFS clones, snapshots, root/privileged access, volume rollback and arbitrary hostile same-UID mutation are outside the cooperative guarantee. The trusted host maintains mount topology. macOS Data/System volume traversal and firmlinks need actual normal-home tests; a pathname being ordinary on macOS is not enough evidence.

Before calling `dirs::cache_dir()`, private `resolve_cache_location() -> Result<PathBuf, Failure>` validates raw `std::env::var_os("HOME")`: present, nonempty, absolute and within the existing raw-component bounds, with no empty, dot, parent, NUL or overlong components. The pinned dirs dependency otherwise falls back to the account database for missing/empty HOME. Only after that guard call dirs::cache_dir, validate its resolved raw absolute components, and allow acquisition; create only the missing final cache-location component and framework-owned suffixes. Resolver-only child tests cover missing/empty/relative/dot/over-bound HOME without any acquisition callback or filesystem I/O. Positive children receive isolated HOME before process spawn; never mutate process-global environment after workers start. This guard applies only to the new cache resolver, not the existing preference resolver or its path contract. Normal location is `~/Library/Caches/qos-internal-install/<manifest-sha256>`. No XDG interpretation, temporary fallback or app-supplied location. This is a **new macOS production root**, since no earlier macOS cache provider exists. Preserve Linux/Windows roots and the existing preference path/bytes exactly. L02's later per-package/workspace migration remains separate. Retain the raw path components and every opened ancestor from `/`; no canonicalize-and-trust or successful-provision leaf substitution. Root-owned safe traversal ancestors may be read-only; commit all writable directory edges that this cache creates/adopts, retaining existing system ancestors as security/identity witnesses. Never attempt to modify or repair a system ancestor.

Private `HeldDirectory` stores `StorageDirectory`, admitted `FileIdentity`, `SecuritySnapshot`, raw name and role. Private `HeldFile` stores ordinary File, identity/security and raw name. `LockedRoot` retains ancestry, the sole `FileLease`, payload and both journal directories, plus sentinel identity/security. Its `check` re-observes every held object and parent-relative binding and the exact four root names. The lease itself is used for sentinel identity/security/metadata; an independent no-follow named read is compared to it. Lock length remains zero. No persistent sentinel unlink. Every Partial/Final holds `Arc<LockedRoot>` through consumption, cancellation and cleanup.

Mac persisted ownership uses a new domain **`qos-macos-volume-fileid-birth-v1`** followed by volume UUID (16 raw bytes), file ID (`u64` LE from the held metadata, cross-checked against `ATTR_CMN_FILEID`), birth seconds (`i64` LE), birth nanos (`u32` LE). Add a shared descriptor-only `PersistentIdentity::of(&File) -> io::Result<PersistentIdentity>` with `canonical_bytes() -> &[u8]` only on macOS, implemented privately beside the volume observer; directories expose `persistent_identity()` through their held handle. This is an opaque observed metadata hint, not serializable authority. Refuse missing/invalid UUID, file-ID disagreement, missing birth data or invalid nanos. Keep full held `FileIdentity` for live comparisons. Qualified restart/remount tests must prove this encoding stable on the exact supported profile; do not persist `st_dev` as the macOS restart key. A copied cache or restored filesystem is not automatically adopted. Linux/Windows domains and all record fields remain unchanged.

## Protocol preservation and final consumption

Keep exact `QOSCP001` encoding/length/checksum, little-endian integers, manifest binding, component roles/names and embedded catalog bytes. New macOS digests occupy the existing opaque 32-byte fields; no version bump or reinterpretation of existing host records. Preserve `.qos-part-<32 lowercase hex>`, `record-{:010}`, permanent `ownership.lock`, `payload`, `journal-0` and `journal-1`. Use framework-private Darwin `getentropy` for 16-byte names, bounded by `MAX_COLLISIONS`; a failed entropy request refuses.

The private provider carries the same state transitions as Linux: classify unknown residue before effects; acquire/check lock; authenticate finals independently first; scan at most three bounded journal survivors per component; bind candidates to held native identities; reject alias/conflicting candidates; create fresh partials exclusively; commit their name before `initialize_fresh`; consume freshness before selection checks; settle the zero journal before cancellation abandons the owner; never recover freshness from decoded history. Preparation receipt revocation is infallible/state-only and cannot clear uncertainty. Readback, journal byte/fingerprint rechecks across visitor, prefix/reset fences, generation limits and payload size bounds remain unchanged.

Journal creation remains direct exclusive creation of its final `record-N` name, then exact bytes, data flush and namespace commit. Failed/torn/unknown records survive conservatively. Retirement deletes only the identities in the freshly validated scan, and commits deletion before acknowledging. Final publication verifies bytes before and after exclusive rename, checks cancellation, commits the destination namespace before journal retirement and opens the read-only final **relative to the held payload directory**, verifying identity and full fingerprint against the existing handle. EEXIST authenticates the existing final; it never overwrites it. Late failure/cancel leaves independently recoverable final bytes and journal evidence with uncertainty/residue retained.

Discard performs a new `Session::prepare`, revalidates ownership, removes payload and commits that removal **before** retiring journals. `release(token)` drops retained Ready ownership without deleting cached data. `resume(selection)` starts a new operation that reauthenticates the manifest, reacquires/revalidates the cache and prepares partials; it cannot reuse an old preparation receipt. Held-reader callbacks use the existing `finish_held_read`: regardless of verification/callback failure, attempt cursor restoration, held-name/security check, complete root check and cancellation check independently; preserve all errors and uncertainty. All handles and lease survive until these checks finish. No postcheck can become a short-circuit `?` chain.

## Production wiring, native tests and exact-worker isolation

Before enabling macOS `cfg(test)` native composition, move `Worker::fixture` and the fixture-only `coordinator/tests.rs` inclusion out of shared `coordinator.rs`. Both wrappers declare an inline `driver` module and `include!("coordinator.rs")`; move the shared file's inner doc header into wrapper documentation/ordinary comments so inclusion is valid. Only `fixture_coordinator::driver` includes `fixture_coordinator/adapter.rs` and its child `tests { include!("coordinator/tests.rs"); }`. These children retain access to driver-private state without exposing Shared/Mailbox/Flags to the crate. The native wrapper alone includes `native_coordinator/test_adapter.rs` and its own native test child; fixture tests are never instantiated under native `platform`. The common worker loop remains single-source. Before spawn, the fixture adapter and its test helper compile-check `platform::CacheRoot::acquire` against the concrete fixture_platform::CacheRoot return type; native adapters check the concrete macos return type. Misplaced inclusion therefore fails compilation before I/O. Real owned-root/fixture-layout tests, exact once-only fixture test inventory and negative native Worker::fixture probes establish isolation before native cfg activation; caller-thread TLS observers cannot prove another worker made zero acquisitions.

Add macOS selection only inside the native wrapper and existing private facade/Engine branches, preserving Linux's current non-test native arrangement. Normal `Client::open_legacy_imager` always selects Native independently of test features; no environment/backend override is read by normal Worker::spawn. The only public dev entrance is `Server::native_macos_client(root: &Path, preference: &Path) -> Client` under macOS+test-support. It exposes neither fault configuration nor receipts. Successful native flow, contention and restart are integration tests; injected failure assertions live in **private native-wrapper unit tests**.

Private `NativeWorkerTestConfig` carries owned `root`, `PreferenceStore`, optional `PreferenceHooks`, one-use `worker_id`, `NativeHooks`, a receipt sender and observation-failed flag. `Worker::native_test(transport, config)` starts the exact native driver thread, then installs an RAII `NativeTestScope` there before `worker_loop`. The scope installs a native root and native checkpoint observer/injector and wraps preference saves using that worker's optional existing `PreferenceHooks`/atomic-file scoped hooks. The public root/preference-only entrance constructs empty hooks; it cannot accidentally call Fixture's preference-fault helper. Root selection is available only within this explicit private scope and is not inherited by normal worker threads. No caller-thread TLS installation counts.

`MacosEvent` contains worker ID, operation ID when admitted, component index, record generation where applicable, positive per-checkpoint occurrence, `MacosCheckpoint`, `EffectState::{BeforeSubmit,SubmittedUnknown,KnownEffect,Committed}`, and typed `ObservedResult::{Success,NativeError{kind,raw_os_error},InjectedError{kind},LostAcknowledgment}`. A bounded receipt channel uses nonblocking try_send and reports the observed event **before** matching failure injection; overflow/disconnect sets a test observation-failed flag, never an invented native I/O error or an indefinite wait; tests correlate it with final Status failure_kind/uncertain/residue and on-disk effects. An unrelated worker/event cannot satisfy the assertion. Preference receipts use the same worker correlation plus existing PreferenceFault and publication/namespace state. Hook matching can name an exact component/generation/occurrence; enum-only “first event anywhere” matching is insufficient.

NativeTestScope restores the previous root, observer, injector and nested hook occurrence counters on normal return, error and unwind, using guard Drop without filesystem cleanup. Test hold handshakes have deadlines; dropped receivers cannot indefinitely park the worker. The controller always closes/joins before deleting its owned fixture directory. Process abort/crash bypasses RAII and is recorded separately; OS handle release and fresh-process recovery, not in-process cleanup assertions, are its subject. Tests cover nested restoration, callback error/panic, spawn/setup failure, cancellation and independent simultaneous workers.

Both crates add **default-off `native-macos-conformance = []`**, independent of test-support; it only admits strict tests and never selects a provider. Every strict native integration target declares Cargo `required-features`; native provider/wrapper unit suites use `all(test,target_os="macos",feature="native-macos-conformance")`. The two manifests' exact feature/test stanzas and dedicated native commands/lane are owned from Task 0 onward, adding each target stanza with its source. Ordinary service fixture suites remain root-independent and execute only Fixture. The dedicated native lane supplies a preadmitted root and requires exact nonzero expected test names/counts; missing root, unexpected ignored test, missing target or zero selected tests fails. Exclusion from an ordinary suite is not native qualification. Constructor probes use a normal library without test-support and repeat with feature unification to prove unchanged production selection. The plan specifies every command and stage of early wiring.

## Evidence, primary sources and unresolved gates

The source facts below were read on 2026-09-19 UTC. Pin upstream revisions/local SDK headers in the implementation handoff before FFI changes. Apple source describes interfaces; it does not certify this application's complete storage protocol.

| Subject | Documented/source fact and its limit |
| --- | --- |
| Exclusive rename | Apple's [rename manual source](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/rename.2) defines descriptor-relative `renameatx_np` and RENAME_EXCL collision behavior. Its [attr.h](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/attr.h) exposes capability validity. Use `renameatx_np(...,RENAME_EXCL)`; support/absence must be observed. Neither establishes crash durability. |
| Flush | Apple's [fsync manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html) distinguishes host flush from device persistence; [current fcntl header](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/fcntl.h) describes F_FULLFSYNC. Apple's [storage guidance](https://developer.apple.com/documentation/xcode/reducing-disk-writes) still calls it best effort. No “fullsync exists, therefore crash-safe” inference is accepted. |
| Actual Rust data call | Local pinned `1.97.1` rust-src `library/std/src/sys/fs/unix.rs:1391` uses `fcntl(F_FULLFSYNC)` for `File::sync_all` on Apple. The existing atomic-file directory method instead calls rustix fsync. The source hash below binds this finding; recheck after toolchain changes. |
| ACL observation | Apple's [descriptor ACL manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man3/acl_get_fd_np.3.html) returns an independent ACL copy, not a lease. [Libc ACL export source](https://github.com/apple-oss-distributions/Libc/blob/main/posix1e/acl_translate.c) supports bounded structural snapshots. Privacy policy and repeated revalidation are our conservative design. |
| Volume/identity observations | Apple's [getattrlist source](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/getattrlist.2) specifies fd lookup, packed lengths and separate volume/common query constraints. [mount.h](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/mount.h) defines local/ownership flags. Check bounds/returned masks; do not use a mount pathname as authority. UUID+file-ID+birth stability remains native evidence to obtain. |
| Lease | Apple's [flock source](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/flock.2) describes advisory/open-file lifetime behavior. The existing Rust/FileLease abstraction is reused, then tested with real contenders and process death. It does not confine a noncooperating owner. |

Native tests use only unique owned directories under an explicitly admitted harness root, with no alterations to user caches, home permissions, volumes or devices. Lack of an admitted fixture is an **open/failed gate**, not a skip or success. Fault hooks acknowledge the exact production checkpoint before injection; restart tests use child processes and assert the exact child test executed. Cooperative race fixtures swap only their own names/ACLs. Full-volume tests need a disposable quota-limited APFS fixture; never fill a shared host disk. Power-loss/kernel-crash tests require an isolated authorized macOS VM/storage rig; process kill is only process-restart evidence.

**Concrete external inputs still missing:** a parent-owned admitted native fixture root and OS/SDK/architecture matrix; disposable APFS quota/case-profile fixtures; a crash rig; and a genuinely signed distribution containing usable internal releases. The current catalog is `schema:1, tag:"unreleased", releases:[]`, so the unmodified production constructor cannot presently perform an authenticated default-catalog download. Test real constructor/preference/close and native worker/provider behavior separately; explicitly labeled localhost selections do not close the default catalog or installed entry gate. Do not add a production catalog override to evade this.

Windows namespace/profile completion, Linux/Windows native conformance and primitive migrations, G02 authenticated per-invoker revocation/lifecycle, L02 scoped-location migration, L03 native/emulated-host architecture and every signed installed OS/architecture qualification remain required. A macOS provider compile or fixture run closes none of them. No permanent Unsupported implementation is an acceptable final substitute.

## Review handoff

Refresh the following hashes after the extraction Task 3 writer finishes; API docs/tests, SDK docs, Imager view grouping, workflow and baseline belong to that writer until parent transfer. Implementers must inspect drift rather than assume this design freezes shared files. Each task reports exact source hashes, touched paths, native-versus-fixture test identities and outstanding gates. Parent performs specification and code review before proceeding between tasks. No tests, native mutations, Cargo/lock edits, staging or commits were performed for this design. Round 1 corrects C1 and I1–I4 from the linked preflight; its immutable review and original document snapshots remain preserved in the SDD workspace.


Snapshot UTC: 2026-09-19T06:50:28.204822+00:00. Git HEAD: `6e5721c6ce73460a282f40b3a265828f5dd28072`; the worktree contains concurrent authorized changes, so HEAD alone does not identify these inputs.

| Input | SHA-256 |
| --- | --- |
| `rust-toolchain.toml` | `da720d52f14003a384901307d4bb4b1794f8b40980d7e9bf9e7aa6b0dae9b8e1` |
| `crates/packaging/install-artifacts/release-catalog.json` | `3aa2c12d568e82557b278a17f9488f55f45e16f40e0c0e3159b369bad44e9f41` |
| `crates/packaging/install-artifacts/src/cache.rs` | `86910bbd5618fccdd80d535d0c17af25ac1894e30852b71012e387333eacd67a` |
| `crates/packaging/install-artifacts/src/cache/checkpoint.rs` | `73800f770f79431b8f771e7c1533a4c7f393567a53daa57dfa029dc991095c73` |
| `crates/packaging/install-artifacts/src/cache/linux.rs` | `8d4fa08684143c2210875b1093eec5a12de35ac1e54094fb0e1bbbe747341fed` |
| `crates/packaging/install-artifacts/src/cache/windows.rs` | `de0e062f297ee33ede29e41c95a74fd7a522b4d042c13738f79105f0d30a7f91` |
| `crates/packaging/install-artifacts/src/cache/native_coordinator.rs` | `2f62dfc398f8c21642838ab7707860ce151199e624a6f4fe012fa86487e28578` |
| `crates/packaging/install-artifacts/src/cache/coordinator.rs` | `e8a3ab8a31d7b111d4245326d68b226a7edb94d76ef5f8cdd9a2e7ff50bfab22` |
| `crates/packaging/install-artifacts/src/cache/fixture_coordinator.rs` | `e19998bd32b75a8993a936eca5bff6ba138378bd0ef762f0d6f716941d71092a` |
| `crates/packaging/install-artifacts/src/cache/fixture_platform.rs` | `96c576cc23444261aed4a85633ba002f1e0202dc40df48a6285500fa1c732d9e` |
| `crates/packaging/install-artifacts/src/cache/fixture_storage.rs` | `605895e2d81fea649c9042670347cedab4a689a18ca577baac61c48385ae6947` |
| `crates/packaging/install-artifacts/src/cache/linux/tests.rs` | `36af8a4c82f744191f5cb60fe9199dc79612affb4d340243782f238996b59b46` |
| `crates/packaging/install-artifacts/src/cache/windows/tests.rs` | `4bfe0d4c6e10b2db186b3cbb9afe09cefbd6098374d2d39d893e05de4e856a82` |
| `crates/packaging/install-artifacts/src/cache/tests.rs` | `eaa6831d3d18a8c2141828dd476e56d3f1687be15ee0d852958f3db100b372fe` |
| `crates/packaging/install-artifacts/src/lib.rs` | `94bd13d79dc89e536b5531fcb0c88a9a98c7a8ce18cdb2711b1a5a5bb21ef6f8` |
| `crates/system/atomic-file/src/lib.rs` | `0bdb0e222a2c7f68c4ea7ed52d1d2fbc9bf138b3c91f1645c7695144a161406b` |
| `crates/system/atomic-file/src/directory.rs` | `fa01cd05b1095041e6733d2536b9fb41425e6f72a4b26d887719f8dd81dbccbb` |
| `crates/system/atomic-file/src/identity.rs` | `2aaae6b6624bf33a15b374f9af9cabc26077419636d880fde7160f8b20283115` |
| `crates/system/atomic-file/src/lease.rs` | `d46e920fd25a2c418ff6d89bad9f702cb139c5f35bfa13a643c7bee9984c5a4a` |
| `crates/system/atomic-file/src/provision.rs` | `3e74af81af46814913e73f253a9b5542245bbac75f1406bea42a37375b24e0d6` |
| `crates/system/atomic-file/src/existing.rs` | `c2bfaaf793bac202ae9cf9c9d0937dbca3419a01f65fe5447779150775a99b68` |
| `crates/system/atomic-file/src/publication.rs` | `6663de9f6b5a628f93efddf05a44ca4b67004cc0360d6b0c2709da926a4daa8d` |
| `crates/system/atomic-file/src/native/unix.rs` | `38e0908201a4c91d10be0f03fe9262b602e02e9afda43aef9c66b1248d3dc4e5` |
| `crates/system/atomic-file/src/native/mod.rs` | `3341a69647084f6b87f4a752204494922e5c02999bf8873f07e22c71fa536257` |
| `crates/system/atomic-file/src/native/windows.rs` | `05bec78cc7e1c95fdd0110c48ec08a7a5b66dc2eb2760439335faded1856a4ca` |
| `crates/system/atomic-file/src/test_support.rs` | `4feac83a62ec8e83c6f1c61969961075bd892cc55abab6635a18146bf962524d` |
| `/Users/xaver/.rustup/toolchains/1.97.1-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/sys/fs/unix.rs` | `02bd45fd5f2e9d8b08122630f8febf15354c1784c7f6550d8cfbfb967cebf30d` |
