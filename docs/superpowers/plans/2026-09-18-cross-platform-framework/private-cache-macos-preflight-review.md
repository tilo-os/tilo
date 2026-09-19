# Private-cache macOS implementation preflight

Reviewed 2026-09-19 UTC. This is a read-only architectural review of
`docs/superpowers/plans/2026-09-18-private-cache-macos.md` (P) against its binding
`private-cache-macos-design.md` (D) and the current shared-storage/cache sources.
Only this report was written. No build, test, native filesystem probe, native
mutation, dependency/lock change, staging, commit or subagent was performed.

**Verdict: revise before implementation.** The required native approach is
sound, and Task 0 correctly blocks an unobserved directory-barrier change.
There is one Critical composition hazard and four Important corrections below.
None is a reason to replace the required macOS backend with Unsupported or a
fixture. Parent owns the rulings and changes; this report does not enact them.

## Findings requiring a parent ruling

### C1 — Critical: enabling the native coordinator also instantiates fixture-only tests against the live native cache

**References:** P:148–164, particularly P:157–159; D:122;
`crates/packaging/install-artifacts/src/cache/coordinator.rs:619` and `:623`;
`src/cache/coordinator/tests.rs:58` and `:85`;
`src/cache/fixture_coordinator.rs:1`; `src/cache/coordinator.rs:417`.

`coordinator.rs` is compiled through both coordinator wrapper modules. Its
`#[cfg(all(test, unix))] mod tests` and its `Worker::fixture` implementation are
currently included wherever that source is included. This is safe for the
existing Linux unit-test composition because the native wrapper is excluded by
`not(test)`. Adding macOS to the native unit-test composition, as P:158 permits,
also includes the fixture test module under `native_coordinator::driver`.

Those tests set `fixture_platform::ROOT` on their worker, then call the current
module's `worker_loop`; `run_selected` calls `platform::CacheRoot::acquire`.
In the native module `platform` will be `macos`, which must ignore fixture TLS
and use `dirs::cache_dir()`. Successful fixture transport can therefore reach
the user's real native cache instead of the test TempDir. The duplicated
`Worker::fixture` method has the same incorrect-provider construction. The
existing macOS workflow actually runs the whole service unit suite
(`.forgejo/workflows/release-imager.yml:690`), so narrowly filtering the new
macOS tests does not contain this hazard.

**Required correction:** make fixture-only construction and its tests owned by
the fixture wrapper, rather than unconditionally included by the shared driver.
For example, transfer `fixture_coordinator.rs` and `coordinator/tests.rs` into
Task 4's ownership and place their fixture adapters/tests in a module whose
`Worker` is unambiguously the fixture driver's Worker. Keep the common worker
loop shared. Place native unit tests under the native wrapper with explicit
worker-local native-root installation. Add a regression proving both wrapper
test compositions choose their respective root/provider, and prove no ordinary
fixture suite instantiates native acquisition. An environment override read by
normal production `Worker::spawn` is not a correction.

### I1 — Important: “explicitly selected” native tests have no opt-in target wiring

**References:** P:28–35, P:43–56, P:68–78, P:145, P:163, P:170;
`crates/system/atomic-file/Cargo.toml:12` and `:31`;
`crates/packaging/install-artifacts/Cargo.toml:8`;
`.forgejo/workflows/release-imager.yml:690`.

Task 0 requires `macos_barriers.rs` to be explicitly selected and to fail if
`TILO_NATIVE_CACHE_TEST_ROOT` is absent. Cargo currently auto-discovers every
`tests/*.rs` target; neither manifest declares native test target requirements
or a native-conformance feature. A macOS-only cfg does not make a target opt-in
on macOS. Thus adding the described file also makes ordinary `cargo test -p
atomic-file` require the external fixture. Later strict native targets have the
same issue; provider unit tests will also be included by the existing unfiltered
service unit-test lane. Deferring workflow fixture wiring to Task 5 leaves this
integration gap in Tasks 0–4.

**Required correction:** choose and record target wiring before Task 0. A
concrete option is a default-off, test-only `native-macos-conformance` feature,
explicit `[[test]]` entries with `required-features` for strict native targets,
and matching cfg on strict provider unit tests. Transfer the two manifests if
using this option; this does not require dependencies or lock changes. Dedicated
native lanes and every required plan command must enable the feature, supply an
admitted root, and assert exact nonzero executed counts; missing root remains
nonzero/open there. The normal constructor/feature-separation tests must still
test a production build without accidentally activating a fixture provider.
Alternatively, supply the required root to every ordinary macOS invocation
starting with the first test addition and explicitly adopt that requirement.
Do not add `#[ignore]`, runtime skip branches, zero-test success or a temporary
directory fallback. Excluding an explicit native lane from an ordinary build
is not evidence that its required conformance has passed.

### I2 — Important: the mandated missing-HOME refusal is not provided by `dirs::cache_dir`

**References:** P:137–138; D:104; workspace `Cargo.toml:405`;
local pinned `dirs-6.0.0/src/mac.rs:5` and `:9`,
`dirs-sys-0.5.0/src/lib.rs:33` and `:44` (hashes below).

Task 3 requires empty/missing HOME to be an explicit error and directs
`CacheRoot::acquire` to use `dirs::cache_dir`. In the actual pinned dependencies,
macOS `cache_dir` appends `Library/Caches` to `home_dir`; empty or missing HOME
causes `home_dir` to fall back to `getpwuid_r(getuid())`. Merely checking the
resolved path for nonempty/absolute/bounded form therefore admits the real
account's home. A negative HOME test that performs acquisition before enforcing
the stated requirement can touch the real cache.

**Required correction:** align the spec and plan explicitly. Preserve P:137's
refusal by validating the raw HOME input before calling `dirs::cache_dir`, then
validate the resolved raw absolute components before any opens or creations.
Keep this private to the new cache resolver; do not change the existing
preference resolver/path contract. Test missing, empty, relative, dot-component
and over-bound HOME through resolver-only child-process cases that assert no
filesystem acquisition is reached. Positive production-constructor children
must set their isolated HOME before process start, not mutate process-global
environment while worker threads exist.

### I3 — Important: native worker fault tests consume an interface the plan does not produce

**References:** P:144, P:154, P:159–160; P:172–176;
`crates/system/atomic-file/src/test_support.rs:52` and `:126`;
`crates/packaging/install-artifacts/src/test_support.rs:52`–`:70`;
`src/cache/coordinator.rs:88` and `:623`.

Task 4 promises native record/final and preference-save failure injection
through an integration test, but its only produced test entrance is
`Server::native_macos_client(root, preference) -> Client`. Atomic-file hooks are
thread-local and will not follow the caller into the spawned worker. The
existing Server preference-fault entrance explicitly constructs a Fixture
client, and MacosCheckpoint plus provider modules remain private. The proposed
root-only native constructor does not install native fault/observation hooks or
preference faults on that owned worker. Reusing the existing Server fault method
would silently exercise DiskProvider instead of the claimed native subject.

**Required correction:** define the worker-local fault handoff before Tasks 3/4:
an explicit private test constructor installs the native root, native checkpoint
observer/injector and optional PreferenceHooks on the exact worker for a bounded
scope, restoring them on every exit. Move the precise fault/status assertions to
private native-wrapper unit tests if retaining the narrow public test-support
entrance; integration tests can exercise that entrance's successful native flow
and real external contention. If the integration test must own these cases,
add a narrowly scoped test-support fault/receipt API and corresponding API
privacy probes. Do not expose Files, providers, preparation receipts or hooks in
the application SDK. Required evidence identifies the native checkpoint,
component/record occurrence, observed effect/result and returned typed
kind/uncertainty, so one event from an unrelated worker cannot satisfy it.

### I4 — Important: an APFS-only receipt does not cover the proposed unconditional shared macOS barrier change

**References:** P:17, P:39–57, P:83, P:111–113; D:34, D:65, D:98, D:102;
`crates/system/atomic-file/src/native/unix.rs:110`–`:132` and `:248`;
`src/provision.rs:357`; `src/lib.rs:14`–`:23`.

Task 0's explicit profile is APFS, but Task 2 replaces common macOS
`Directory::sync`, which is used by publication/provision/existing-commit
consumers without the cache's APFS-only admission. Existing macOS shared privacy
admission has no APFS restriction at all, and `Privacy::Inherit` adds none.
Testing the cache fixture and its APFS ancestors therefore does not establish
that the unconditional change preserves every previously admitted shared
consumer profile. The readonly-ancestor gate is valuable and necessary, but is
not the entire shared-owner compatibility domain.

**Required correction:** make Task 0/2's acceptance table enumerate actual
shared-consumer profiles separately from cache admission, including directory
types and readonly traversal ancestors. Authorize a common barrier change only
for the observed shared domain. If a required consumer profile rejects the
candidate, revise the shared strategy/admission contract with primary/native
evidence before the common change; do not silently downgrade a failed fullsync,
copy a private cache-only syscall implementation, or revoke required support as
a way to make the new backend pass. Record unobserved profiles as open rather
than extrapolating APFS results. No claim here is that the candidate fails on
APFS: native feasibility was intentionally not probed.

## SDD task self-consistency table

Each task is represented. “Conditional” means its intended contract is coherent
but the named preflight correction or gate must be resolved before execution.

| Task | Its specified producer, tests and later use | Internal-consistency result |
| --- | --- | --- |
| 0 | Creates only the barrier target/support helper and a source/profile-bound syscall receipt; tests both actual calls and readonly/common-caller ancestry; changes no storage implementation (P:37–57). | Conditional: correct feasibility-before-rollout ordering, no false crash claim. I1 is missing target/lane wiring; I4 limits what a positive receipt can authorize. Native success is not established by this review. |
| 1 | Creates observation/security/relative-child APIs, macOS persistent identity and the creation phase types before Task 2 extends them; tests real held objects, no-follow, exclusive residue, ACL drift and lease identity (P:59–79). | Consistent subject to Task 0 decision and I1. New `NamespaceError` arrives in the same task that first returns it. Linux and macOS implementations are required for the stated Unix surface; no Apple type may appear in Linux signatures or unconditional Windows exports. Existing privacy policy stays unchanged. |
| 2 | Consumes held APIs plus phase types; creates no-replace rename, owned retirement, retained retry and shared barriers; exercises real operations and production checkpoints (P:81–113). | Conditional on I4 and reviewed Task 0 evidence. Distinct creation/unlink states and PublicationError are correctly separated; clone/retry custody is feasible. Every hook needs an explicit effect/phase mapping, described below, rather than generic `map_err` conversion. |
| 3 | Uses unchanged checkpoint bytes/traits, creates complete native root/journal/component ownership, freshness and reader postchecks, and tests the production provider (P:115–146). | Conditional on I2 and I1. File split matches responsibilities. Direct exclusive record creation correctly preserves failure residue; no placeholder Provider is acceptable. Checkpoint definitions must include the worker-installation contract consumed in I3. |
| 4 | Selects the native provider in normal construction, produces explicit native-worker/fixture-transport entrance, tests constructor, worker, failures and privacy (P:148–164). | Not internally complete: C1 makes the current shared fixture-test inclusion unsafe after native cfg widening; I3 leaves the specified fault integration tests without their required interface. Production constructor evidence remains distinct from fixture-catalog worker execution. |
| 5 | Consumes frozen test binaries/checkpoints and installed Client, produces independent compile/native/restart/full-volume/crash/default-catalog/installed receipts (P:166–182). | Conditional on I1/I3 wiring. Evidence categories and nonzero/missing-input policy are consistent. It may deliver an honest open-gate ledger, but cannot claim qualification or N02 completion without the named external evidence. |

## SDD shared-file/interface pair table

All 15 unordered task pairs are represented, including evidence interfaces and
indirect consumers. This avoids treating a transitive test/evidence dependency
as independent work merely because product files differ.

| Tasks | Shared file/interface: what one produces and the other consumes | Finding / handoff requirement |
| --- | --- | --- |
| 0 / 1 | `tests/support/mod.rs`; admitted native root and syscall/profile receipt → real observation/child tests and pre-implementation decision. | Sequential helper ownership is explicit. I1 requires manifest/lane wiring; do not upgrade Task 0 into APFS certification using Task 1's later observer without retaining which facts were observed when. |
| 0 / 2 | Candidate native directory sequence/source-profile receipt → common `Directory::sync` and namespace checkpoints. | Strong explicit gate and source/toolchain/profile drift rule (P:83); I4 requires shared-consumer profile coverage. Negative readonly-ancestor result must prevent the blanket change. |
| 0 / 3 | Accepted barrier strategy plus safe root/profile → provider commits of partials, records, finals and directories. | Correct indirect dependency through Tasks 1/2. Provider cannot bypass a failed candidate with its own flush, a fixture provider or permanent Unsupported. |
| 0 / 4 | Safe fixture-root contract and barrier acceptance → native-worker roots and isolated constructor HOME. | I1 governs test admission; I2 prevents account-database fallback; C1 prevents fixture tests reaching normal live acquisition. Preference save uses the shared common ancestor path and must be represented in barrier evidence. |
| 0 / 5 | Feasibility receipt (not persistence) → qualification ledger/source and profile identity. | Consistent separation. Retain original syscall outcomes and hashes; later crash or installed rows cannot retroactively turn absent feasibility observations into a pass. |
| 1 / 2 | `directory.rs`, `lib.rs`, `namespace.rs`, `native/unix.rs`, `native/unix/macos.rs`; held creation/observations → owned mutations/retry. | Correct phase-type dependency. Reuse one validator/native helper; preserve safe snapshot changes as observable. Error-only failed creation retains residue and must not acquire a cleanup destructor. |
| 1 / 3 | `StorageDirectory`, `FileIdentity`, `SecuritySnapshot`, `FilesystemInfo`, `PersistentIdentity`, sole-handle FileLease observation → HeldDirectory/HeldFile/LockedRoot. | Contracts support retained ancestry and lease postchecks. Cache exact modes/APFS/canonical spelling stay private; OwnerControlledDirectory must not broaden generic OwnerOnly. Mac canonical bytes stay 36 bytes; Linux/Windows digest domains unchanged. |
| 1 / 4 | Framework-internal observation/held-child APIs and strict native test setup → native worker and API-surface probes. | Unix exports need explicit cfg parity; platform types remain private. I1/C1 prevent a compile/test-feature choice from redirecting native work to real user locations. |
| 1 / 5 | Actual native ACL/profile/identity/lease tests → APFS/architecture/restart/remount receipts. | Consistent. Parser fixtures supplement actual descriptor tests; unavailable foreign-UID/device fixtures remain open. Cross-compilation cannot certify native ABI execution or identity persistence. |
| 2 / 3 | `commit_namespace`, owned rename/remove, PublicationError/NamespaceError and exact mutation checkpoints → journal publication/retirement, final publication and discard. | Correct semantic fit. Explicitly map phase to retained residue/poison before converting to Failure. Final namespace must commit before journal deletion, payload deletion before discard journal retirement. |
| 2 / 4 | Shared barriers/phase errors/checkpoints → preference gating and native-worker status coalescing. | I3 needs actual worker-local installation of hooks. C1 affects which provider consumes them. A preference failure must precede transport/cache effects; native mutation uncertainty must survive typed status conversion. |
| 2 / 5 | Native operation boundary events and commit retry custody → fault/restart/full-volume/crash driver. | Consistent only with distinct pre-submit, submitted, known-effect, barrier and completed observations. I3 supplies exact-worker acknowledgments; source/binary hashes must bind the actual hook implementation. |
| 3 / 4 | `cache.rs` plus macos CacheRoot/Partial/FinalHandle/Component, fault hooks → native platform alias, engine and test entrance. | C1 and I3 are blocking interface/composition gaps. Preserve unchanged checkpoint contract and borrowed SelectedManifest lifetimes. Root overrides must be available only to the explicit owned test worker. |
| 3 / 5 | Native provider identities, QOSCP001 records, retained root and MacosCheckpoint → restart/crash/admission matrix. | Consistent. Crash evidence must include zero-record/reset/final/retirement boundaries and persistent identity stability. A process kill or constructed record cannot prove device persistence or mint freshness. |
| 4 / 5 | Production Client, explicit native worker entrance, API privacy tests and transferred workflow paths → real constructor, fixture-native and signed installed rows. | Consistent evidence taxonomy, conditional on C1/I1/I3 fixes. Empty real catalog keeps default-catalog/installed download open. Parent must transfer extraction-owned workflow/API files and rehash them before editing. |

## Shared policy, custody and error-phase review

The division between generic storage and cache policy is appropriate. Shared
APIs should observe security/profile and perform held relative I/O; the provider
should require local writable ownership-enforced APFS, exact 0700/0600 modes,
known names, journal semantics and complete root custody. Existing
`StorageDirectory::open` remains trusted-host ambient admission; cache ancestry
must start with a retained root and use relative no-follow edges, not call that
ambient constructor at each component. A safe owner-controlled root ancestor is
not an owner-only private cache directory.

Task 1 must implement the new Unix API on Linux as well as macOS. The existing
Linux profile classifier includes tmpfs/OverlayFS for generic permission
admission (`native/unix.rs:110`), while the old cache admits only ext4/XFS/Btrfs
(`cache/linux.rs:195`). Preserve that distinction. Linux bounded access/default
ACL snapshots can use the already pinned rustix `fgetxattr`; no Apple UUID/ACL
layout belongs in Linux public types. Explicitly cfg the new security exports
and `FileLease::security_snapshot` with their implementations; D's note only
spells out cfg for directory methods. `PersistentIdentity` and its directory
method are macOS-only. Windows behavior and compilation must remain intact.

`FileLease` consumes its sole File (`lease.rs:57`–`:90`) and currently exposes
metadata/identity without cloning. Adding security observation against that
same private handle is implementable. An independently opened sentinel name can
be compared against it; it must never be mistaken for the locked handle or used
to replace the held lease. `Arc<LockedRoot>` on every Partial/Final plus ordinary
payload clones is compatible with the existing protocol. Root/sentinel security
and identity must remain alive until all independently attempted reader
postchecks finish, including callback failure/cancellation. D:118 correctly
forbids a short-circuit `?` chain for those four postchecks.

The error contracts are viable but require the following exact implementation
table at the Task 2 → Task 3 handoff:

| Observed boundary | Required classification/custody |
| --- | --- |
| Invalid component, failed security/preflight, or proven exclusive-create collision before an effect | NamespaceState::Unchanged; no deletion or repaired permissions. |
| Native create succeeded; later open/type/privacy/snapshot check failed | ChangedDurabilityUnknown with exact named residue preserved, even if no File was returned. |
| Mutation was submitted and effect cannot be established | NamespaceUnknown / PublicationUnknown, preserving possible names and poison. An injected errno alone is not proof of no effect. |
| Exclusive rename returns native EEXIST | NotPublished/AlreadyExists; preserve both names and authenticate an existing final separately. A synthetic EEXIST after rename success must not enter this branch. |
| Rename known successful, destination identity/privacy validated, barrier fails | PublishedDurabilityUnknown with held file/parent/name/expected identity/privacy retained for commit-only retry. |
| Retry sees a replacement | Refuse, preserve replacement, never issue rename/unlink or infer authority from ambient paths. |
| Unlink succeeded, barrier fails | ChangedDurabilityUnknown; no destructor cleanup or claim of clean journal state. |
| Observation after final successful barrier | Mark whether observation-only or simulated lost acknowledgment; do not silently treat a test-hook error as a native syscall error. |

`PublicationError::retry_commit` already retains an opaque native guard
(`publication.rs:93`), but the current Unix guard only holds file/parent and
calls metadata plus parent sync (`native/unix.rs:254`). It needs the additional
owned-name identity/privacy validation for the new rename use. A private guard
variant can preserve existing publication semantics as P:113 requires. The
provider must explicitly inspect new typed phases before `Failure::io`, whose
current mapping otherwise preserves the I/O kind but has no knowledge of
namespace effects (`error.rs:20`, `:47`). No diagnostic-string parsing is needed.

## Darwin ABI and primary-source checks

No ABI implementation or native call was executed. The following are source
checks against local **MacOSX26.5.sdk** headers, not a claim that this is the SDK
used by an eventual build or an OS-floor qualification. Both named architectures
in Task 5 are LP64; use the exact corresponding declarations.

| Subject | Checked requirement and result |
| --- | --- |
| `fgetattrlist` | Local `usr/include/unistd.h:760` declares the LP64 options parameter as `unsigned int`, not `unsigned long`; `sys/attr.h:81` has 16-bit bitmap count/reserved followed by five 32-bit groups. Use correct C integer widths, zero reserved, bitmapcount 5 and checked return length. |
| Packed returned attributes | D:65/D:108 and P:74's separate volume/common requests are necessary: UUID/capabilities volume query includes ATTR_VOL_INFO and returned masks; file ID/birth query is separate. Native attributes are packed at 4-byte boundaries even when values are 64-bit. Do not transmute an ordinary aligned Rust result struct. Unsupported omitted attributes or invalid capability bits are refusal/None, never zero-filled apparent proof. [Apple getattrlist contract](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/getattrlist.2). |
| Capability | Local `sys/attr.h:120`–`:128`, `:360`, `:397` distinguish the interface capability and its valid mask. Exclusive rename is bit `0x00080000` in the interfaces group. Its presence is observation, not durability evidence. [Apple attr.h](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/attr.h). |
| Exclusive rename | Local `sys/stdio.h:37`, `:53`: RENAME_EXCL is `0x00000004`; renameatx_np takes two int dirfds, two C-name pointers and unsigned-int flags. The header's availability declaration does not qualify the project's minimum supported OS. The native EEXIST contract supports no-overwrite collision handling. [Apple rename manual](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/rename.2). |
| ACL bytes | Local `sys/acl.h:206`, `:211`: `acl_copy_ext` and `acl_size` use signed `ssize_t`; reject negative/over-4096 lengths before allocation and validate exported size. Apple's implementation clears the external buffer and includes ordered qualifiers/flags/rights, supporting structural snapshots. Keep absence distinct from any failed observation. [Apple ACL export implementation](https://github.com/apple-oss-distributions/Libc/blob/main/posix1e/acl_translate.c). |
| Flush | Pinned Rust std `unix.rs:1391`–`:1397` implements File::sync_all with F_FULLFSYNC on Apple; current Directory::sync uses rustix fsync. Task 0's two actual calls therefore test different calls. Apple's current manual describes device flush semantics and supported filesystem/hardware limits; it is not proof this application's directory composition succeeded on the target. [Apple fcntl manual](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/fcntl.2). |
| Lease | Pinned std `unix.rs:1543` uses nonblocking flock for this path. Advisory locks and duplicate/fork ownership behavior support retaining the single lease File, not treating a name reopen or duplicate as a separate authority. Actual contender/death tests remain required. [Apple flock manual](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/flock.2). |

The plan already requires local SDK/source hashes and primary-reference pinning
before FFI changes. Keep that requirement. Moving upstream `main` links in the
planning text must not be the sole ABI version identifier. The local immutable
hashes below bind what this review actually read; they do not certify a future
SDK/toolchain or replace arm64/x86_64 execution.

## Native, fixture, default-catalog and installed evidence

The planned distinctions are correct. The real constructor currently cannot
start a usable default-catalog release: the actual catalog remains
`{"schema":1,"tag":"unreleased","releases":[]}`. Its hash below matches D.
Constructor/preference/close evidence proves those executed paths only. A
native APFS provider driven by localhost Server uses real storage and fixture
transport/catalog. It must be labeled accordingly, even though the storage
subject is valuable production code. A signed installed package with a usable
embedded release and real same-origin assets is a separate observation.

Task 0 support of a syscall is separate from Task 2 production reachability,
Task 3 complete provider behavior, Task 4 real worker composition, process
restart, remount/reboot, disk-full and crash persistence. No native runs were
performed by this review, and no external fixture/rig or installed package was
inferred to exist. Missing inputs remain open with an explicit owner/input;
Windows namespace/profile work, S01/S02, G02, L02 and L03 are still independent
required program gates. The parent may accept completed implementation while
these observations remain open, but cannot relabel that as complete required
native support qualification.

## Binding input snapshots and drift

Git HEAD during review: `6e5721c6ce73460a282f40b3a265828f5dd28072`.
The worktree has concurrent authorized changes; HEAD is not a substitute for
per-file hashes. All **27** source inputs in D:150–178 matched their recorded
SHA-256 on initial inspection. Final recheck results and additional binding
inputs are appended below. Extraction-owned paths must still be transferred
and rehashed after their owner finishes; this review grants no shared ownership.

Final source-snapshot recheck UTC: `2026-09-19T06:59:10.573953+00:00`.

| Design input | Current SHA-256 | Against design snapshot |
| --- | --- | --- |
| `rust-toolchain.toml` | `da720d52f14003a384901307d4bb4b1794f8b40980d7e9bf9e7aa6b0dae9b8e1` | Match |
| `crates/packaging/install-artifacts/release-catalog.json` | `3aa2c12d568e82557b278a17f9488f55f45e16f40e0c0e3159b369bad44e9f41` | Match |
| `crates/packaging/install-artifacts/src/cache.rs` | `86910bbd5618fccdd80d535d0c17af25ac1894e30852b71012e387333eacd67a` | Match |
| `crates/packaging/install-artifacts/src/cache/checkpoint.rs` | `73800f770f79431b8f771e7c1533a4c7f393567a53daa57dfa029dc991095c73` | Match |
| `crates/packaging/install-artifacts/src/cache/linux.rs` | `8d4fa08684143c2210875b1093eec5a12de35ac1e54094fb0e1bbbe747341fed` | Match |
| `crates/packaging/install-artifacts/src/cache/windows.rs` | `de0e062f297ee33ede29e41c95a74fd7a522b4d042c13738f79105f0d30a7f91` | Match |
| `crates/packaging/install-artifacts/src/cache/native_coordinator.rs` | `2f62dfc398f8c21642838ab7707860ce151199e624a6f4fe012fa86487e28578` | Match |
| `crates/packaging/install-artifacts/src/cache/coordinator.rs` | `e8a3ab8a31d7b111d4245326d68b226a7edb94d76ef5f8cdd9a2e7ff50bfab22` | Match |
| `crates/packaging/install-artifacts/src/cache/fixture_coordinator.rs` | `e19998bd32b75a8993a936eca5bff6ba138378bd0ef762f0d6f716941d71092a` | Match |
| `crates/packaging/install-artifacts/src/cache/fixture_platform.rs` | `96c576cc23444261aed4a85633ba002f1e0202dc40df48a6285500fa1c732d9e` | Match |
| `crates/packaging/install-artifacts/src/cache/fixture_storage.rs` | `605895e2d81fea649c9042670347cedab4a689a18ca577baac61c48385ae6947` | Match |
| `crates/packaging/install-artifacts/src/cache/linux/tests.rs` | `36af8a4c82f744191f5cb60fe9199dc79612affb4d340243782f238996b59b46` | Match |
| `crates/packaging/install-artifacts/src/cache/windows/tests.rs` | `4bfe0d4c6e10b2db186b3cbb9afe09cefbd6098374d2d39d893e05de4e856a82` | Match |
| `crates/packaging/install-artifacts/src/cache/tests.rs` | `eaa6831d3d18a8c2141828dd476e56d3f1687be15ee0d852958f3db100b372fe` | Match |
| `crates/packaging/install-artifacts/src/lib.rs` | `94bd13d79dc89e536b5531fcb0c88a9a98c7a8ce18cdb2711b1a5a5bb21ef6f8` | Match |
| `crates/system/atomic-file/src/lib.rs` | `0bdb0e222a2c7f68c4ea7ed52d1d2fbc9bf138b3c91f1645c7695144a161406b` | Match |
| `crates/system/atomic-file/src/directory.rs` | `fa01cd05b1095041e6733d2536b9fb41425e6f72a4b26d887719f8dd81dbccbb` | Match |
| `crates/system/atomic-file/src/identity.rs` | `2aaae6b6624bf33a15b374f9af9cabc26077419636d880fde7160f8b20283115` | Match |
| `crates/system/atomic-file/src/lease.rs` | `d46e920fd25a2c418ff6d89bad9f702cb139c5f35bfa13a643c7bee9984c5a4a` | Match |
| `crates/system/atomic-file/src/provision.rs` | `3e74af81af46814913e73f253a9b5542245bbac75f1406bea42a37375b24e0d6` | Match |
| `crates/system/atomic-file/src/existing.rs` | `c2bfaaf793bac202ae9cf9c9d0937dbca3419a01f65fe5447779150775a99b68` | Match |
| `crates/system/atomic-file/src/publication.rs` | `6663de9f6b5a628f93efddf05a44ca4b67004cc0360d6b0c2709da926a4daa8d` | Match |
| `crates/system/atomic-file/src/native/unix.rs` | `38e0908201a4c91d10be0f03fe9262b602e02e9afda43aef9c66b1248d3dc4e5` | Match |
| `crates/system/atomic-file/src/native/mod.rs` | `3341a69647084f6b87f4a752204494922e5c02999bf8873f07e22c71fa536257` | Match |
| `crates/system/atomic-file/src/native/windows.rs` | `05bec78cc7e1c95fdd0110c48ec08a7a5b66dc2eb2760439335faded1856a4ca` | Match |
| `crates/system/atomic-file/src/test_support.rs` | `4feac83a62ec8e83c6f1c61969961075bd892cc55abab6635a18146bf962524d` | Match |
| `/Users/xaver/.rustup/toolchains/1.97.1-aarch64-apple-darwin/lib/rustlib/src/rust/library/std/src/sys/fs/unix.rs` | `02bd45fd5f2e9d8b08122630f8febf15354c1784c7f6550d8cfbfb967cebf30d` | Match |

Additional review inputs (current bytes, not native execution evidence):

| Input | SHA-256 |
| --- | --- |
| `docs/superpowers/plans/2026-09-18-private-cache-macos.md` | `7da6363797464d2689baa01467f0a80da90cdbd75658c395481d56a48b2dae06` |
| `docs/superpowers/plans/2026-09-18-cross-platform-framework/private-cache-macos-design.md` | `44cacaef7e79377077a702807eede3fa157804f9ddf4a4eb73d2fde04541e73e` |
| `crates/system/atomic-file/Cargo.toml` | `18c585e525d25294001002893605a10596861ab88386edc1afa785726123da07` |
| `crates/system/atomic-file/tests/parent_provision.rs` | `3427396f8e95803b44be0028d9f3ca6652078e540f833418130ea3e2cf5c98f8` |
| `crates/packaging/install-artifacts/Cargo.toml` | `1211990603b548017bed6090fbf3f3c35e3d51b9dafd682923439695e9cdb252` |
| `crates/packaging/install-artifacts/src/cache/coordinator/tests.rs` | `2cb0a720077f2917b12f086adaf37b19177f6d71d561b0dd7b4791d10f082ec2` |
| `crates/packaging/install-artifacts/src/test_support.rs` | `2d7f903c3857a14bf31be06c7ee8aa02d4b33d084cb004c09c7110f8fdf99f2a` |
| `crates/packaging/install-artifacts/src/preference.rs` | `05560cf6314d0cf310c006f13daa1a3dc7fa188a86ff26fe49e10826d2bb961b` |
| `crates/packaging/install-artifacts/src/error.rs` | `8f93dfbba238da24ae7595f5b2fb6bb728cb4eb352cb9c7c2e6d272938f5475e` |
| `.forgejo/workflows/release-imager.yml` | `a2aaf28ac8946925cde980d531c6a26c79a0822c4af3d5d7839a547f80c1dc58` |
| `/Users/xaver/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/dirs-6.0.0/src/mac.rs` | `40de270051a116ca35f95f065db3a5d8020bc911619846a9a67c222973b2e5ad` |
| `/Users/xaver/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/dirs-sys-0.5.0/src/lib.rs` | `b519e22cfb5ccc4b307aed57f67a66641660f2b29496f00db761b7c49307e2dd` |
| `/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk/usr/include/unistd.h` | `8d535079658f063bcef358ef4c1c1b52b8e88dacfc22cbf4db0f6bf8abe40637` |
| `/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk/usr/include/sys/attr.h` | `5118b9245bc932bc32dcc60084b1e40d70bb5479a360b31cdbf188ef866deefe` |
| `/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk/usr/include/sys/acl.h` | `9511f84f0abe1e108e10979900d4fea8567534aef78f0984f7050c49f6c29ff7` |
| `/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk/usr/include/sys/mount.h` | `45872741bb916b92bddd2e562d2047b552af1f68269081a0aeb58a3e5a2b8a51` |
| `/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk/usr/include/sys/fcntl.h` | `805fd8c695f8e5e1c327b6852382cc5533738bbfd8f18bc11f850531166e4fe8` |
| `/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk/usr/include/sys/stdio.h` | `a8078ae1cba1a46a66d0de6fe7f13bbc969cdb7e78fc2d4b9a5c5cdaaf16126b` |
