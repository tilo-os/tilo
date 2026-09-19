# Private installer artifact service: source ownership design

Date: 2026-09-18. Architectural design for the already authorized framework migration. Read with [framework-contract.md](framework-contract.md), [reconnaissance](private-cache-extraction-recon.md), and the [execution plan](../2026-09-18-private-cache-extraction.md). This design settles the six reconnaissance choices. It is a source-ownership and caller-migration milestone; it does not certify native hosts or close G02, L02, S01/S02, N02, or installed-distribution evidence.

No implementation begins until the parent transfers the exact Imager/service/SDK/catalog/workflow paths. Current Imager ownership, the separate UI five-file hold and folder-service ownership restrictions remain effective. This documentation task changes no product source, executes no builds and performs no git operations.

## Decision and alternatives

Create the headless framework crate **`tilo-install-artifacts`** at **`crates/packaging/install-artifacts`**. Expose its bounded semantic interface as **`ui::install_artifacts`**, behind the semantic **`install-artifacts`** SDK feature, identically on every host. Keep cache/checkpoint, HTTP transport, selected-manifest authority, native verifier and native names private. Keep pure target format/catalog parsing in distinct portable modules in the same crate. This avoids a premature extra format crate while permitting a later ordinary dependency split without native authority changes.

Alternative 1: move only native providers and retain all transport/coordinator code in Imager. Rejected: the app would need the current private `PreparedPrefix`, mutable files, observers and preparation receipts, or a larger scoped-transfer redesign. Alternative 2: replace the cache with atomic-file/runtime-fs/package downloads now. Rejected: none implements the same journal survivor, prefix/reset, held-final, ACL/snapshot, filesystem-admission and consumption contract. Chosen approach: relocate the coherent current algorithms first, then make the separately reviewed preference persistence correction. Primitive consolidation remains mandatory work with explicit acceptance dependencies; it is not accomplished by renaming this crate.

The crate is not a new permission service or generic filesystem. Existing `atomic-file` owns shared privacy/publication/lease primitives; existing runtime and HostContext own grants/identity/lifetimes. `folder-service` owns the independently approved migration/security/connected-folder closure. This extraction never edits it, exposes its private modules or copies them. An independently authorized peer may change its private native chain after the N03 handoff; record those source identities and the peer receipt separately, coordinate coherent build windows, and never attribute peer changes to this extraction or assert the whole service stayed byte-identical.

## Catalog ownership and authentication

Move the **single authoritative catalog**, unchanged, from `apps/imager/release-catalog.json` to `crates/packaging/install-artifacts/release-catalog.json`. `catalog.rs` has the only production `include_str!("../release-catalog.json")`. No build environment override, runtime catalog path, public trusted-catalog registration, alternate origin parameter or caller-supplied trust boolean is added. Both the internal downloader and Imager's media offer parser read those same embedded bytes.

Preserve current signature provenance exactly: trust comes from the genuinely signed distribution containing these bytes. A local development build and a catalog digest alone are not cryptographic release evidence. Package signing remains a release pipeline gate. The service does not invent another signing key or silently accept unsigned remote catalog updates.

The release workflow's four old source/embed references move together (`release-imager.yml:253,311,464,600`). The catalog wire format, downloaded release asset name and `release-catalog.py --copy-to` validation remain unchanged. The Python script's destination argument is already generic. Its tests gain an embedding destination assertion. No dist script, signing operation or publication is needed for the source move.

Move only `release.rs`'s portable catalog records, shape/schema/all-entry validation helpers and internal-release authentication into the service. Full mixed-catalog validation is retained: an invalid media entry on another architecture must still reject internal enumeration. Do not weaken this into “ignore unfamiliar entries.” `catalog_data` returns **untrusted portable data**, including arbitrary caller-parsed data; it never mints selections. Public record/field spelling stays exactly `Catalog {schema, tag, releases}`, `CatalogRelease::{Media, Internal}`, `MediaRelease`, `MediaProvenance`, `InternalMetadata`, and their current fields/tags. `present_tag`, profile/provenance/digest/origin validators retain current semantics. Field visibility promotion applies only to metadata, not proofs. Serde object/duplicate/unknown-field behavior remains unchanged; add only metadata Clone/Eq support needed by opaque selection cloning/comparison.

`catalog.rs` privately validates the embedded catalog, converts its internal entries into the existing private `InternalRelease`, and returns opaque `ReleaseSelection` wrappers. `InternalRelease::verify_manifest`, `SelectedManifest::checked`, original exact bytes, same-origin HTTP rules and selected release/architecture/purpose match all remain inside the service. There is no public `From<InternalMetadata>`, deserializer or builder for a `ReleaseSelection`. Public parsing of `InternalBundle` is metadata validation, not authentication. Tests get a distinct feature-gated fixture path.

Imager retains `media_releases`, `releases_for_host`, conversion to `TiloRelease`, full-OS capability policy, exactly-one-offer selection, review UI, its existing media downloader and `Source` construction. Its internal catalog wrapper delegates to `bundled_selections`. Preserve current internal host filtering using the build architecture inside the framework for this source milestone; the app no longer reads `std::env::consts::ARCH` for this path. Native/emulated-host selection is tracked under L03 and is not quietly changed here. Other users of `apps/imager/src/host.rs` stay with their existing owner.

## Exact source layout

Paths under the new crate are relative to `crates/packaging/install-artifacts`.

| Source | Destination / change |
| --- | --- |
| `apps/imager/release-catalog.json` | `release-catalog.json`, byte-for-byte move, one production embed |
| `apps/imager/src/setup/internal_bundle.rs` | `src/bundle.rs`, entire file and 26 tests initially retained; native `unix`/`windows` remain private nested modules; `src/format.rs` narrowly reexports portable `InternalBundle`, `ComponentView`, `MAX_MANIFEST_BYTES`, `validate_internal_bundle` |
| `apps/imager/src/setup/internal_download.rs` and `/tests.rs` | `src/transport.rs` and `src/transport/tests.rs`; private module including selected manifest/transfer authority, preserve algorithms |
| `apps/imager/src/setup/internal_cache.rs` | `src/cache.rs`; private root, scoped reader and postchecks |
| `internal_cache/{checkpoint,linux,windows,tests}.rs` | `src/cache/{checkpoint,linux,windows,tests}.rs`, intact algorithms/tests |
| `internal_cache/{linux,windows}/tests.rs` | `src/cache/{linux,windows}/tests.rs`, intact native tests |
| `internal_cache/coordinator.rs` | `src/cache/coordinator.rs`, one worker-driver source, no public raw consumer; public DTO definitions extracted into `src/download.rs` |
| `internal_cache/coordinator/tests.rs` | `src/cache/coordinator/tests.rs`; its private `fixture` provider block moves to `src/cache/fixture_platform.rs` for explicit dev-support instantiation |
| no old file | `src/cache/{native_coordinator,fixture_coordinator}.rs`: two private module wrappers around the same coordinator source, described below |
| `release.rs` records/helpers and internal selection methods | `src/catalog_data.rs` (portable data/validation) and `src/catalog.rs` (private embedded admission/selection authentication); move their pure tests into these modules; app media projection tests remain app-side |
| no old file | `src/{lib,download,error,preference,test_support}.rs`; semantic facade, typed data/error shell, preference I/O, restricted fixtures |
| `internal_download_flow.rs` preference I/O | `src/preference.rs`; app retains SelectionKey schema/encode/decode and product workflow |
| `crates/app/tilo-app/{Cargo.toml,src/lib.rs}` | optional dependency and explicit module export only; add `src/install_artifacts.rs` facade wrapper; do not `pub use service::*` |
| `apps/imager/src/setup/{mod,view,internal_download_flow}.rs` | SDK imports; remove only internal-download host cfg guards; preserve host-specific neighboring features and install refusal |
| `apps/imager/src/release.rs` | SDK parser/metadata imports, embedded catalog byte accessor and opaque internal selection alias; retain media policy |
| `apps/imager/src/setup/internal_bundle.rs` after move | small compatibility reexport of portable format and one-shot verifier; no native implementation |
| `apps/imager/src/setup/internal_download.rs` after move | delete; no repository production consumer needs its public mutable-File transport API; moved tests own those calls |
| root Cargo/lock, new manifest, release workflow, platform baseline | exact membership/dependency/consumer/test changes only |

The final `src/lib.rs` declares `mod bundle; mod cache; mod catalog; mod download; mod error; mod preference; mod transport; pub mod catalog_data; pub mod format;`, plus `#[cfg(feature = "test-support")] pub mod test_support;` and `#[cfg(all(test, not(feature = "test-support")))] mod test_support;`. The same helper source is private during default unit tests and public only in explicitly enabled dev-support builds. Task 1 installs modules as their files are created within its cohesive Part A/B deliverable; `mod preference` and its storage implementation are added only in Task 2. Task 1 may declare the final preference DTOs in download.rs for Status, with no preference I/O or placeholder storage module. Public reexports are enumerated below. No `cache`, `transport`, provider, raw identity, preparation, `Ready`, backend or worker-driver module is public.

The relocation changes roots (`crate::setup::internal_*` to private crate siblings), restricted visibility (`pub(in crate::setup)` to at most `pub(crate)`), DTO placement and literal test filters explicitly. Never normalize visibility changes away in relocation evidence. Record unchanged bodies, moved test attributes/ignore reasons and every residual semantic diff separately.

### Production and fixture instantiation

Keep one coordinator source with `use super::platform;` and absolute `crate::cache::{Session, HeldReadSeek}` imports. `native_coordinator.rs` chooses Linux/Windows privately and contains `#[path = "coordinator.rs"] pub(crate) mod driver;`. Its cfg is `any(windows, all(target_os = "linux", not(test)))`, preserving Windows native coordinator unit builds. `fixture_coordinator.rs` imports `crate::cache::fixture_platform as platform` and instantiates the same file with the same `#[path]`; it is compiled under `all(unix, any(test, feature = "test-support"))`. This preserves the previous Unix unit-test coordinator fixture while allowing Imager's dev dependency to use it without selecting a fixture in production.

Move `Token`, `Phase`, `Status` and `Cancellation` declarations out of that source to private/public-appropriate shared modules so the two drivers have identical facade DTO types. `Client` has a private `Engine` enum with the native driver and an explicitly selected test-support driver. **Enabling test-support never changes `Client::open_legacy_imager` to use a fixture.** In a normal library build (including integration tests), Linux construction with that feature still uses Linux; on macOS the normal constructor still reports current unavailable support; only `test_support::Server::client` chooses the portable fixture. In the Linux library unit-test build only, native_coordinator is intentionally absent to preserve the existing fixture test arrangement: `availability()` returns UnsupportedHost and `open_legacy_imager()` returns ErrorKind::Unsupported, never a fixture. An explicit unit test asserts this. Production-constructor claims must come from integration tests linked to the normal library, with and without test-support; unit-test cfg results are not host qualification. Native provider tests remain directly against native providers. Private scoped-consumer tests remain in the fixture driver and continue to receive the original borrowed readers; the app fixture API never exports them.

## Exact semantic interface

Signatures describe the public API; opaque structs have private fields, no serde and no public constructors beyond the listed methods. `ReleaseSelection`, `Token` are `Clone + Debug + Eq + PartialEq` (selection equality compares the exact catalog identity fields); `Client` is neither Clone nor serializable. Keep `Progress` and the cache's phase/status fields spelled as below so caller changes are finite.

```rust
pub fn bundled_catalog_json() -> &'static str;
pub fn bundled_selections() -> Result<Vec<ReleaseSelection>, Error>;
pub fn catalog_architecture() -> &'static str;
pub fn availability() -> Availability;
pub fn verify_local_components(
    bundle: &format::InternalBundle,
    directory: &std::path::Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), Error>;

impl ReleaseSelection {
    pub fn release_id(&self) -> &str;
    pub fn architecture(&self) -> &str;
    pub fn manifest_sha256(&self) -> &str;
}
impl Client {
    pub fn open_legacy_imager() -> Result<Self, Error>;
    pub fn preference(&self) -> PreferenceState;
    pub fn start(&self, selection: ReleaseSelection, saved_key: Vec<u8>) -> Result<Token, Error>;
    pub fn resume(&self, selection: ReleaseSelection) -> Result<Token, Error>;
    pub fn pause(&self, token: &Token) -> Result<(), Error>;
    pub fn cancel(&self, token: &Token) -> Result<(), Error>;
    pub fn release(&self, token: &Token) -> Result<(), Error>;
    pub fn discard(&self, token: &Token, selection: ReleaseSelection) -> Result<Token, Error>;
    pub fn status(&self) -> Option<Status>;
    pub fn close(&mut self) -> Result<(), Error>;
}
```

No public `consume`, install, staging, raw media, verified-reader factory, cache path, backend selector or application-supplied release URL. The one-shot path verifier is the old public compatibility capability, not a grant or retained read proof; G02 must replace its production use with granted input before final certification. There is no current repository production caller to migrate. Close sets shutdown/cancel out of band and joins the exact owned thread once; Drop performs the same operation but cannot report an error. Its filesystem/callback waits retain the existing cooperative latency limitation. A second close is successful and does not join twice; new submissions afterward return `Closed`.

```rust
pub enum Availability { Available, UnsupportedHost }
pub enum Phase {
    SavingPreference, Authenticating, Acquiring, Preparing, Downloading,
    Ready, Consuming, Consumed, Paused, Cancelled, Failed, Released, Discarded,
}
pub struct Status {
    pub token: Token,
    pub phase: Phase,
    pub committed: [u64; 2],
    pub progress: Option<Progress>,
    pub finals: [bool; 2],
    pub residue: bool,
    pub uncertain: bool,
    pub error: Option<String>,
    pub failure_kind: Option<ErrorKind>,
    pub preference_commit: Option<PreferenceCommit>,
}
pub struct Progress {
    pub operation: u64,
    pub component: usize,
    pub phase: TransferPhase,
    pub received: u64,
    pub verified: u64,
}
pub enum TransferPhase {
    Prefix, Requesting, Receiving, Retrying, Resetting, Verifying, Complete, Failed, Cancelled,
}
pub enum ErrorKind { InvalidInput, PermissionDenied, Unsupported, Busy, Stale, Closed, Failed }
pub struct Error { kind: ErrorKind, message: String, uncertain: bool }
impl Error {
    pub fn kind(&self) -> ErrorKind;
    pub fn message(&self) -> &str;
    pub fn uncertain(&self) -> bool;
}
```

Scalar enums derive `Clone, Copy, Debug, Eq, PartialEq`; Status/Progress derive Clone/Debug (Progress also Copy). PreferenceCommit derives Clone/Copy/Debug/Eq/PartialEq; PreferenceFailure and PreferenceState derive Clone/Debug/Eq/PartialEq and are not Copy. `Error` implements Display/Error and Clone; no error string is parsed to choose a category. Keep low-level String diagnostics during the mechanical move. In the semantic adaptation task, introduce the crate error through native I/O helpers (`PermissionDenied`/`Unsupported`/`WouldBlock`), explicit native lock-conflict branches (`Busy`), explicit stale-token branches, parser boundaries (`InvalidInput`) and provider admission refusals (unsupported filesystem vs denied owner/permissions). Arbitrary leftover diagnostic strings map to `Failed`, never success or absence. Multi-error `finish_held_read` retains every existing diagnostic and sets uncertainty; cancellation determines terminal phase without destroying those diagnostics. Existing provider protocol acknowledgements and poisoning remain unchanged. Native detailed taxonomy not represented by these categories remains a diagnostic, not a guessed native code. Phase/DTO placement and Error/Result origin adaptation are explicit residual semantic changes; they do not authorize rewriting checkpoint encodings, identity digests, HTTP authentication, reset/recovery ordering or native validation algorithms.

### Preference I/O and its ordered worker integration

The app retains SelectionKey schema 1, its exact bounded JSON rules and existing path compatibility. The service knows only a maximum 4096-byte opaque local preference value at the fixed legacy path `dirs::data_local_dir()/qos/setup/internal-download-selection.json`. It never derives selection authority from these bytes. No generic public key/path store or caller-provided profile identity is added.

```rust
pub enum PreferencePublication { NotPublished, Published, PublishedDurabilityUnknown, PublicationUnknown }
pub enum PreferenceNamespace { Unchanged, Durable, CreatedDurabilityUnknown, NamespaceUnknown, Unproven }
pub enum PreferenceDurability { Durable, NamespaceUnproven }
pub struct PreferenceCommit { pub durability: PreferenceDurability }
pub struct PreferenceFailure {
    pub kind: ErrorKind,
    pub message: String,
    pub publication: PreferencePublication,
    pub namespace: PreferenceNamespace,
}
pub enum PreferenceState {
    Loading,
    Loaded(Option<Vec<u8>>),
    Failed(PreferenceFailure),
}
```

Private `PreferenceStore::{load,save}` return `Result<Option<Vec<u8>>, PreferenceFailure>` and `Result<PreferenceCommit, PreferenceFailure>`. Resolve an unavailable/relative/empty data root as an explicit pre-effect error; no cwd/temp fallback. Load never creates anything: no-follow child read through `StorageDirectory`, bound before/allocation and after read; missing is None, oversized is None, actual I/O error is Failed, schema-malformed remains an app decode-to-None decision. Use existing atomic-file trusted-host semantics; this is not a new hostile-path grant.

Unix save uses `atomic_file::provision_directory(parent, Privacy::Inherit)`, namespace-control validation on the held leaf and `StorageDirectory::publish(name, bytes, Replace, OwnerOnly)`. Map `ProvisionError::state` and `PublicationError::state` precisely; retain unknown effects and never compensate by deletion. No ignored directory sync. Windows save uses the current `atomic_file::write_creating_parent_with_mode(path, bytes, 0o600)` wrapper, maps embedded publication state, and **always** reports successful content publication as `NamespaceUnproven`: that wrapper does not prove newly created ancestor persistence. A Windows error without an embedded publication state is pre-publication; namespace effects remain conservatively `Unproven`. Unqualified publication profiles return a typed failure; do not bypass atomic-file with the old tempfile writer. Completion of Windows namespace qualification/required profiles remains S01, not a permanent supported-host omission.

The worker reads the preference once before accepting a cache operation and exposes PreferenceState. Client creation, load, save and cache operations do not block paint/click callbacks. Client construction starts one owned worker; storage root resolution/read happens there. `start` checks the 4096 bound synchronously, then enqueues the selected release plus key bytes; the worker reports SavingPreference, saves, records PreferenceCommit, and only then authenticates/acquires the download. Save failure emits Failed with the preference publication/namespace facts retained in PreferenceState, performs no network or cache action, and leaves the prior saved app key unchanged. `resume` and `discard` do not rewrite the preference. Coalesced status retains PreferenceCommit for the whole token, so skipping an intermediate SavingPreference update cannot lose the acknowledgement.

The app disables start/resume until preference load settles, decodes Loaded bytes locally, and keeps existing stale-catalog re-resolution. It updates its `saved` key only upon seeing that token's PreferenceCommit. A NamespaceUnproven commit permits the same unprivileged download the previous Windows code permitted but adds a persistent product warning that the selection may not survive power loss; it is not displayed as durable. Failed saves remain failures even if bytes may already be published. Cancellation during saving preserves the commit/failure facts and never starts authentication after cancellation. This improves truthful persistence without inventing a false Windows barrier or blocking all previously supported downloads as an incidental location change.

## HostContext, legacy compatibility and final authority

Actual interfaces inspected: `HostContext::prepare` (`ui/src/application.rs:92`) receives app definition, canonical RecordStore/views, device/instance, `Rc<RefCell<PermissionContext>>`, author and ActionContext. `wake_handle` is public; canonical identity fields and callback binding are private. Teardown closes control custody, sets closing and closes wake scope before dropping roots (`:680,:744`). `WakeHandle::detached()` stays active and is not authentication. Imager currently uses `LaunchSpec::new_with_args`, never receives a HostContext, and has no Send live-invocation permission lease. Do not claim otherwise or manufacture one from `author`, ActionContext or a permission snapshot.

**Decision for this executable milestone:** use the explicitly named `Client::open_legacy_imager` compatibility entrance with only the fixed embedded catalog, existing current-user storage scope and existing unprivileged download behavior. No new authority is enabled. Do not add a global client registry, fake host context or detached wake ownership. Preserve existing widget-owned client Drop/join and status polling; a future wake adapter must use the existing wake system, not a second action bus. This entrance carries an explicit G02 removal item, not a permanent SDK ideal. It has no arbitrary app/principal/workspace/path/backend argument.

**Dependent G02 integration, not executable in this source slice:** add `HostContext::install_artifacts(&self) -> Result<ui::install_artifacts::Client, Error>` through `ui/src/application.rs` once Imager uses the component host path and the real invocation scope exists. Host preparation must mint it from the same canonical app/workspace/device/instance and owner lifetime; close must signal cancellation before root drop and retain cleanup ownership until joined. Existing WakeHandle can route/drop late delivery but cannot grant operations. Per-operation native and result-publication checks require G02's authenticated invoker/delegated scope; absent that interface this adapter is not declared implemented. A read-only lifecycle adapter alone must not close G02. The exact source-slice public API is complete above; there is no placeholder new permission type in it.

## Security, error, persistence and platform acceptance

The reconnaissance's complete invariant section is normative, including `QOSCP001`, little-endian record layout/checksum/binding, native identity digest domains, raw names, existing cache roots, journal/payload separation, bounds, reset-before-truncate, fresh zero settlement, independent readback, no-replace behavior, held-reader postchecks, bounded retained state and unknown-residue retention. No algorithmic simplification is authorized by relocation.

Native Linux remains cooperative against same-UID mutation and requires qualified ext4/XFS/Btrfs. Windows remains cooperative, holds native security/sharing/identity guards and admits its existing NTFS/ReFS cache profile, but namespace_boundary is only validation. Unix one-shot bundle verification is not a full hostile-ancestry walk. No support claim can exceed those actual properties. Missing persistent cache on production macOS returns UnsupportedHost through the service; this is a recorded intermediate state, **not final N02 completion**. Required macOS implementation, Windows durable namespace/profile support, L03 emulated-host facts, L02 root migration, S01/S02 consolidation and native installed evidence remain required delivery work with separate owners/dependency gates.

Do not replace this work with permanent Unsupported backends. Do not infer that cross-compilation or macOS fixture-worker tests implement the missing macOS native cache. No staging/install/raw-media capability is added; current no-staging consumer refusal and absence of public consume are tested.

## Tests and restricted fixtures

All 101 cache test attributes, 26 bundle tests and 49 transport tests move with their production algorithms; app flow's 16 tests and view/release policy tests remain consumer tests. Preserve the five existing cache ignore reasons and inventory their native requirements. Change only the two exact subprocess filters to `cache::linux::tests::native::lock_probe` and `cache::windows::tests::native::lock_probe`; parent tests must assert a nonzero exact test execution, not exit zero from no matching test.

The service's dev-only `test-support` feature exports the following **exact** fixture interface, by moving the existing transport server helper into `src/test_support.rs` and the coordinator provider fixture into `src/cache/fixture_platform.rs`. Server fields and transport conversion remain private. Server/localhost/client fixtures are `#[cfg(unix)]`, matching the existing Imager consumer fixture suites; portable manifest/catalog fixture functions compile on every target:

```rust
pub fn manifest_value() -> serde_json::Value;
pub fn manifest_bytes(value: &serde_json::Value) -> Vec<u8>;
pub fn response(status: u16, headers: &str, body: &[u8]) -> Vec<u8>;
pub fn selection_from_catalog(raw: &str) -> Result<ReleaseSelection, Error>;
pub enum PreferenceFault { BeforePublish, BeforeCommit }
// Server is opaque, with the existing private owned server fields.
impl Server {
    pub fn new(responses: Vec<Vec<u8>>) -> Self;
    pub fn on_request(responses: Vec<Vec<u8>>, hook: impl FnMut() + Send + 'static) -> Self;
    pub fn selection(&self, manifest: &[u8]) -> ReleaseSelection;
    pub fn client(&self, root: &std::path::Path, preference: &std::path::Path) -> Client;
    pub fn client_with_preference_fault(
        &self, root: &std::path::Path, preference: &std::path::Path,
        fault: PreferenceFault,
    ) -> (Client, std::sync::mpsc::Receiver<PreferenceFault>);
    pub fn client_with_preference_hold(
        &self, root: &std::path::Path, preference: &std::path::Path,
    ) -> (Client, std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>);
    pub fn requests(&self) -> Vec<String>;
    pub fn consumptions(&self) -> u64;
}
```

Task 2 adds the two preference fault/hold methods. Atomic-file hooks are thread-local: install them inside the owned worker around the actual save, never on the test caller thread. The fault method observes and acknowledges the chosen production checkpoint before injection; the hold method acknowledges BeforeCommit, then waits at most ten seconds for its release channel. The test must receive the acknowledgement or fail. Neither method exposes a generic callback or storage handle. Enable atomic-file/test-support only through this dev feature and dev dependencies; normal publication remains identical.

The Server declaration is intentionally opaque; implement a named-field struct with private fields, never a public unit-struct constructor. Expose no transport object, `File`, provider, consumer callback, receipt, raw handle or held-reader factory. Imager's dev dependency enables service test-support; normal SDK builds and installed packages must not. Fixture selection is deliberately powerful test-only input and not provenance evidence. Production constructors remain unaffected even when Cargo feature unification enables support during tests.

Normal and support consumer probes require positive imports and intended privacy failures (E0603/E0451/E0616/E0599 as appropriate). Probe private cache/provider/PreparedPrefix/transport/native modules, selection construction/deserialization, consume/handle getters, backend constructors and normal-build test-support absence. Resolve rustc artifacts from Cargo JSON and pair rlib/rmeta; E0463 or arbitrary build errors never count as privacy success. Preserve app tests' actual production facade path and verify old module paths cannot retain native implementation.

## Self-review and approval boundary

Alternatives, ownership, catalog provenance, app/server APIs, DTO spelling, actual host limitations, preference phases, fixture selection and seven continuing program dependencies are explicit. The plan assigns each implementable requirement to a testable task. No genuine product choice remains blocked on user input. Host authority/native guarantees are evidence and dependency blockers, not unanswered design questions. Parent-coordinated source ownership is the only prerequisite to starting the source plan. Any departure from fixed catalog admission, private proof boundaries, preserved formats or staged native guarantees requires a new design review.

## Additional design-input identity snapshot

The reconnaissance manifest identifies the extraction closure. The following snapshot binds the additional lifecycle/catalog/fixture/build inputs inspected for this design; it is not a source freeze or a claim that concurrent authorized work cannot change them. Refresh affected inputs at handoff and attribute other owners’ edits separately. Recorded UTC: 2026-09-19T01:19:56.439723+00:00.

| File | SHA-256 |
| --- | --- |
| `ui/src/application.rs` | `1aa375c9aecf4e472b684c32c56ddd69099bd13b335b114a728f5dc6284d3819` |
| `ui/src/wake.rs` | `89ae24ef1595a8439bb23f77bea19fd76023f268af11b95e53712ae1b44cbea7` |
| `ui/src/permissions/mod.rs` | `aa4afe70d2d1685a553e236c9dae9c74999f44ee1e9eab2c3edaa10be9c46925` |
| `apps/imager/src/main.rs` | `c7d9f5d12736233f6756458e9e1b90c72496190313d884c9c4fab980eaaa464a` |
| `apps/imager/src/host.rs` | `ee20f935412af6ef895dc546ca62f6db232b1462f57082a73c6d846fd906c932` |
| `apps/imager/src/release.rs` | `5651e897a0cf2d46645b895b30d70534097248c76064c42d1bf2a29795158894` |
| `apps/imager/release-catalog.json` | `3aa2c12d568e82557b278a17f9488f55f45e16f40e0c0e3159b369bad44e9f41` |
| `apps/imager/packaging/release-catalog.py` | `21d3f666d2e2552daf2719e2592bc29a6d9ecdb038230dd0d8da510f372e106c` |
| `apps/imager/packaging/test_release_catalog.py` | `cb111ece3709b4ca9fca851b0ae7f868be6fe9f4238b70203f5650ea0a4f78f8` |
| `.forgejo/workflows/release-imager.yml` | `a8a1ba4a03e333df966ca23d942ddc4203fc1becd5379d0be33ba4c3a3eb40ed` |
| `crates/app/tilo-app/Cargo.toml` | `df814cdaa902350b6930ec88657c8125975a1685d8bffae4ac476593b72c986a` |
| `crates/app/tilo-app/src/lib.rs` | `416c19d0631384eec6ac048ad4a263036447915596c8a46e1b9f7389f89dd7b6` |
| `crates/system/atomic-file/Cargo.toml` | `18c585e525d25294001002893605a10596861ab88386edc1afa785726123da07` |
| `crates/system/atomic-file/src/test_support.rs` | `4feac83a62ec8e83c6f1c61969961075bd892cc55abab6635a18146bf962524d` |
| `tools/check_app_platform_contract.py` | `7f2512e7976d62965bb1fbbdaa753c83afbb49a92efb13184b200f4b095fee8e` |
| `tools/tests/test_app_platform_contract.py` | `effd7e7320af1cb189fb6c673120bacb4f6111f725980c09549ae4a1667d644b` |
