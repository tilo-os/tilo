# Private Installer Artifact Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (already selected for this migration) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Parent coordinates exact source ownership before dispatch; this plan does not authorize concurrent edits to held files.

**Goal:** Move Imager's authenticated internal-artifact download/cache and held bundle verification into a headless framework service, preserve private authority and persisted bytes, migrate consumers, and correct preference persistence with honest native outcomes.

**Architecture:** `tilo-install-artifacts` owns one embedded catalog, portable validation/authentication/HTTP modules, private cache/checkpoint/native verifier, and a semantic SDK facade. Imager retains offer policy, preference JSON and presentation. Existing atomic-file owns shared storage primitives; folder-service remains untouched, while full G02 and native qualification remain separately required.

**Tech Stack:** Rust edition 2024; existing serde/serde_json, sha2, reqwest blocking/Rustls, cap-std/cap-primitives/cap-fs-ext 3.4.6, dirs, libc, windows-sys 0.61.2, atomic-file and tempfile test fixtures. Preserve existing Cargo.lock versions/checksums.

**Spec:** [Private-cache extraction design](2026-09-18-cross-platform-framework/private-cache-extraction-design.md), with [source reconnaissance and hashes](2026-09-18-cross-platform-framework/private-cache-extraction-recon.md) and [framework contract](2026-09-18-cross-platform-framework/framework-contract.md).

## Global constraints

- New crate: `crates/packaging/install-artifacts`, package `tilo-install-artifacts`; SDK module `ui::install_artifacts`, semantic feature `install-artifacts` on every host.
- No framework-to-app dependency, app-relative include, public provider/PreparedPrefix/ownership constructor, raw handle, File-returning API, backend selector or arbitrary trusted catalog factory.
- One authoritative embedded catalog at `crates/packaging/install-artifacts/release-catalog.json`; no runtime/build-env catalog override. Development embedding is not a signature.
- Preserve `QOSCP001`, native identity digest domains, raw Unix/UTF-16 names, cache root `qos-internal-install`, payload/journal layout and all limits/algorithms described in the spec.
- Preserve `data_local_dir()/qos/setup/internal-download-selection.json` and app-owned SelectionKey schema 1. Never use cwd/shared-temp fallback.
- `folder-service` is outside extraction ownership: no edits, private implementation dependency, visibility widening or copied security/chain implementation. The parent may authorize a separate N03 peer chain change; preserve its independent receipt and coordinate coherent builds.
- No raw-media/staging/install activation. No public consume operation. Keep the existing no-staging consumer refusal.
- Normal production construction never selects a fixture because `test-support` is enabled.
- Source milestone only: G02, macOS production cache, Windows namespace durability/required profiles, S01/S02 consolidation, L02 migration, L03 native host facts and installed native evidence remain mandatory follow-on work. Do not substitute permanent Unsupported implementations for final functionality.
- Never set `CARGO_TARGET_DIR`, regenerate Cargo.lock, weaken assertions, add ignore annotations, silently skip unobservable native checks, or classify E0463 as a privacy success.
- No builds run concurrently with a held source writer. Parent owns integration/commit decisions; workers freeze reviewable changes and do not stage/commit shared WIP without explicit path ownership and parent instruction.

---

## Execution boundaries and evidence

Use the exact current shared WIP bytes after handoff, not a reconstructed HEAD. The recon's 49 hashes identify its read snapshot; changed inputs require a source refresh before work. Keep public review artifacts under `docs/superpowers/plans/2026-09-18-cross-platform-framework/`: `private-cache-relocation-report.md`, `private-cache-relocation-review.md`, and `private-cache-source-manifest.json`. Large command logs/snapshots belong in the parent-designated task evidence directory, with their SHA-256 identities linked from that report. No evidence directory contains substitute product source on the normal build path.

Task 1 has two implementation parts and one reviewable deliverable: the complete catalog/authentication/format/transport/cache move with compiling migrated consumers. Part A may run focused service checks while Part B is in progress; no intermediate source tree is handed off or approved as the completed relocation. Task 2 changes preference persistence and adds the final asynchronous preference API. Task 3 closes semantic/private API and packaging/source-evidence gates. Task 1's temporary `start(selection)` signature is intentionally replaced by final `start(selection, saved_key)` in Task 2; no later task depends on the temporary signature. Do not release the intermediate state as the finished milestone.

Read `docs/architecture.md`, `docs/headless-testing.md`, `ui/HIG.md`, relevant `plan/architecture.md`/`plan/security.md`, and `DEVELOPMENT.md` before source work. No UI core source change is necessary in these three tasks; the real HostContext/G02 dependency is specified after them.

### Task 1: Cohesive catalog, format, native cache and caller relocation

#### Part A: Portable catalog/format ownership and sealed embedded selections

**Files:**

- Create `crates/packaging/install-artifacts/{Cargo.toml,release-catalog.json,src/lib.rs,src/catalog_data.rs,src/catalog.rs,src/bundle.rs,src/format.rs,src/error.rs}`.
- Modify `Cargo.toml`, `Cargo.lock`, `crates/app/tilo-app/{Cargo.toml,src/lib.rs}`; create `crates/app/tilo-app/src/install_artifacts.rs`.
- Modify `apps/imager/Cargo.toml`, `apps/imager/src/release.rs`, `apps/imager/src/setup/internal_bundle.rs`, `apps/imager/src/setup/internal_download.rs` for the portable split in Part A and complete opaque-selection caller migration in Part B of this same task.
- Delete `apps/imager/release-catalog.json` after the sole embed is moved.
- Modify `.forgejo/workflows/release-imager.yml`, `apps/imager/packaging/test_release_catalog.py` for the catalog location. The generator's wire format and downloaded asset name stay unchanged.
- Tests: moved bundle/catalog unit tests; new `crates/packaging/install-artifacts/tests/catalog_surface.rs`; existing Imager media/catalog tests; Python release-catalog suite.

**Interfaces:**

- Consumes existing strict `parse_catalog`, `InternalRelease::verify_manifest`, `InternalBundle` and the catalog bytes.
- Produces final `format::{InternalBundle, ComponentView, MAX_MANIFEST_BYTES, validate_internal_bundle}`, `catalog_data::parse_catalog(&str) -> Result<Catalog, String>`, all current catalog metadata DTO fields, `bundled_catalog_json() -> &'static str`, `bundled_selections() -> Result<Vec<ReleaseSelection>, Error>`, `catalog_architecture() -> &'static str`, and ReleaseSelection's three read-only getters.
- `InternalRelease`, `SelectedManifest` and proof constructors are private/crate-private. Execute Parts A and B within the same source ownership and review unit before the first app integration build. Do not expose a proof constructor to make the intermediate app compile. No other worker receives that incomplete tree as a dependency.

- [ ] **Capture input identities and exact tests before editing.** Save bytes/hashes for the explicit move/split list; inventory test/ignore attributes and two literal subprocess filters. Parse Cargo.lock into package identities and dependency edges for later comparison. Record the current folder-service manifest and separately authorized peer receipt. Prove this extraction has no folder-service edits; do not demand global unchanged hashes across authorized peer changes. Bind each build to the actual coherent source inputs.

```python
from pathlib import Path
import hashlib, json, re
base = Path('apps/imager/src/setup')
paths = [base/'internal_bundle.rs', base/'internal_download.rs',
         base/'internal_download/tests.rs', base/'internal_cache.rs',
         *sorted((base/'internal_cache').rglob('*.rs'))]
rows = [{'path': str(p), 'sha256': hashlib.sha256(p.read_bytes()).hexdigest(),
         'tests': len(re.findall(r'#\[test\]', p.read_text())),
         'ignores': re.findall(r'#\[ignore[^\n]*', p.read_text())} for p in paths]
assert sum(r['tests'] for r in rows if 'internal_cache' in r['path']) == 101
print(json.dumps(rows, indent=2))
```

- [ ] **Add the portable surface tests first.** The integration test below uses only metadata APIs. Add a private catalog unit test `embedded_selection_cannot_be_minted_from_parsed_data`: parse mutated metadata successfully where format-valid, but obtain live selections only by `bundled_selections`; the private test proves each result equals its embedded record and selected original bytes reject digest/length/purpose/architecture mismatch. Preserve existing invalid-foreign-media-entry rejection, not merely internal-entry validation.

```rust
use tilo_install_artifacts::{bundled_catalog_json, bundled_selections, catalog_data};

#[test]
fn published_unreleased_catalog_has_no_authenticated_selections() {
    let catalog = catalog_data::parse_catalog(bundled_catalog_json()).unwrap();
    if catalog.tag == "unreleased" {
        assert!(catalog.releases.is_empty());
        assert!(bundled_selections().unwrap().is_empty());
    } else {
        assert!(catalog.tag.starts_with("imager-v"));
    }
}

#[test]
fn duplicate_and_positional_catalogs_are_rejected() {
    let duplicate = r#"{"schema":1,"schema":1,"tag":"unreleased","releases":[]}"#;
    assert!(catalog_data::parse_catalog(duplicate).is_err());
    assert!(catalog_data::parse_catalog(r#"[1,"unreleased",[]]"#).is_err());
    let valid = catalog_data::parse_catalog(r#"{"schema":1,"tag":"unreleased","releases":[]}"#).unwrap();
    assert_eq!(valid.schema, 1);
    assert_eq!(valid.tag, "unreleased");
    assert!(valid.releases.is_empty());
}
```

The parser assertions are behavior checks, not selection-authority proof; the mandatory negative selection-constructor probe is in Task 3. For this pure relocation, preserved production test bodies and source normalization are the main algorithm-preservation evidence. Do not count a missing package/API error as a behavioral RED result.

- [ ] **Move the portable code and single catalog.** Preserve bundle algorithms and all 26 embedded tests in `src/bundle.rs`. Keep the native verifier private pending Part B’s facade; use `src/format.rs` only to reexport portable functions/types. Extract the exact catalog record and validation closure from `release.rs`, leaving media projection/downloading in Imager. Keep original error text and serde object-shape/duplicate handling. Add only needed metadata Clone/Eq support for opaque selections; do not derive Deserialize on selections.

```rust
// src/catalog.rs -- production trust source, no override.
const BUNDLED_CATALOG: &str = include_str!("../release-catalog.json");
pub fn bundled_catalog_json() -> &'static str { BUNDLED_CATALOG }
pub fn catalog_architecture() -> &'static str { std::env::consts::ARCH }

// InternalRelease is the moved private type; its fields never become public.
#[derive(Debug)]
pub struct ReleaseSelection { pub(crate) release: InternalRelease }
pub fn bundled_selections() -> Result<Vec<ReleaseSelection>, crate::Error> {
    let catalog = crate::catalog_data::parse_catalog(BUNDLED_CATALOG)
        .map_err(crate::Error::invalid_input)?;
    Ok(catalog.releases.into_iter().filter_map(|entry| match entry {
        crate::catalog_data::CatalogRelease::Internal(metadata)
            if metadata.arch == catalog_architecture() =>
            Some(ReleaseSelection { release: InternalRelease { metadata } }),
        _ => None,
    }).collect())
}
```

`Error::invalid_input(message: impl Into<String>) -> Error` is **pub(crate)**; the public Error contract is the design's three getters plus Display/Error. Implement selection Clone/Eq against its exact internal catalog fields; no public builder or raw URL getter is needed. `catalog_data::InternalMetadata` stays plain data and cannot call this private constructor.

- [ ] **Wire the one semantic feature and fixed embed path.** SDK Cargo: `install-artifacts = ["dep:tilo-install-artifacts"]`, dependency path `../../packaging/install-artifacts`, optional true; `src/lib.rs` declares `#[cfg(feature = "install-artifacts")] pub mod install_artifacts;`. Imager enables that same feature in both existing SDK dependency declarations. Add normal dependency edges only; do not resolve new versions.

```rust
// SDK src/install_artifacts.rs, Task 1 exports.
pub use tilo_install_artifacts::{ReleaseSelection, Error, ErrorKind,
    bundled_catalog_json, bundled_selections, catalog_architecture};
pub use tilo_install_artifacts::{catalog_data, format};
```

- [ ] **Change exactly four workflow catalog paths and add a packaging assertion.** All become `crates/packaging/install-artifacts/release-catalog.json`; downloaded `release-catalog` directories/assets stay as they are. In `test_release_catalog.py`, run the existing copy command against a temporary destination with the new directory structure, assert destination bytes equal the validated source, and assert the workflow has no remaining old source/embed path. Do not modify dist scripts held by the other writer.

- [ ] **Check the portable part before continuing within this task.** Run `cargo test -p tilo-install-artifacts --lib catalog --locked`, `cargo test -p tilo-install-artifacts --lib bundle --locked`, `cargo test -p tilo-install-artifacts --test catalog_surface --locked`, and `python3 -m unittest discover -s apps/imager/packaging -p test_release_catalog.py`. Native bundle tests on the host count only for that host. Record source hashes and test names, then complete Part B before the task review. Parent reviews the portable/catalog and native residual diff together against the compiling migrated app.

#### Part B: Cache, transport and worker move with semantic callers

**Files:**

- Create the following under `crates/packaging/install-artifacts/src`: `transport.rs`, `transport/tests.rs`, `cache.rs`, `cache/checkpoint.rs`, `cache/linux.rs`, `cache/windows.rs`, `cache/tests.rs`, `cache/linux/tests.rs`, `cache/windows/tests.rs`, `cache/coordinator.rs`, `cache/coordinator/tests.rs`, `cache/fixture_platform.rs`, `cache/native_coordinator.rs`, `cache/fixture_coordinator.rs`, `download.rs`, `test_support.rs`.
- Modify service `lib.rs`, `error.rs`, `catalog.rs`, Cargo manifest; SDK `src/install_artifacts.rs`; app `release.rs`, `setup/{mod.rs,view.rs,internal_download_flow.rs,internal_bundle.rs}` and Cargo dev dependencies. Delete the original cache subtree and internal_download implementation/test file.
- Native dependencies stay target-scoped; cap-* is needed by Windows bundle verification, libc by Unix, windows-sys features mirror the moved source. No UI/rendering/app dependency in the service. Keep remaining Imager native dependencies where other modules use them.
- Tests: all moved 101 cache/49 transport tests; app flow/view/media tests; new service `tests/api_surface.rs`; private fixture/normal-constructor distinction test.

**Interfaces:**

- Consumes Part A's private InternalRelease/format/authentication and exact catalog source.
- Produces Client's design methods **except** preference/preference-aware start, which Task 2 adds. Temporary Task 1 signature: `start(&self, selection: ReleaseSelection) -> Result<Token, Error>`. `resume` delegates to a fresh authenticated start; app preference stays as existing until Task 2. Other final signatures and status fields are installed now; `preference_commit` stays None until Task 2. Define preference DTOs in download.rs now as needed by Status; do not declare `mod preference` or create a stub store until Task 2 installs the real implementation.
- No public consumer. Driver constructor internally retains the old scoped callback, with production callback returning the existing protected-staging refusal. Public `release` retains finals; `discard` removes only freshly revalidated partials.

- [ ] **Move files before rewriting algorithms.** Preserve byte snapshots; apply only exact module-root changes first, excluding visibility from normalization. Extract coordinator DTOs into download.rs and fixture provider block into fixture_platform.rs, preserving body identity. All previous `pub(in crate::setup)` items become at most `pub(crate)` inside private service modules; narrow inner visibility remains narrow.

```python
from pathlib import Path
old=Path('apps/imager/src/setup'); new=Path('crates/packaging/install-artifacts/src')
moves={'internal_download.rs':'transport.rs',
       'internal_download/tests.rs':'transport/tests.rs',
       'internal_cache.rs':'cache.rs'}
moves.update({str(p.relative_to(old)): str(p.relative_to(old)).replace('internal_cache/', 'cache/', 1)
              for p in (old/'internal_cache').rglob('*.rs')})
for source, target in moves.items():
    dest=new/target
    dest.parent.mkdir(parents=True, exist_ok=True)
    assert not dest.exists(), dest
    (old/source).rename(dest)
```

Rewrite references to `crate::setup::internal_bundle`, `internal_download`, `internal_cache` to `crate::bundle`, `crate::transport`, `crate::cache`; `crate::release::InternalRelease` becomes `crate::catalog::InternalRelease`. Resolve relative imports explicitly. Do not global-replace persisted strings, error strings, env names or encoded bytes. Preserve normal module semantics and cfg guard meaning during extraction.

- [ ] **Install explicit native/fixture driver wrappers.** Both instantiate the single `coordinator.rs`; update its imports to shared DTOs plus `super::platform`. Preserve the old unit-test selection: Linux/macOS Unix unit tests use the fixture driver; Windows unit builds use the native Windows driver. Therefore native wrapper cfg is `any(windows, all(target_os = "linux", not(test)))`; fixture wrapper cfg is `all(unix, any(test, feature = "test-support"))`. The fixture server/type API itself is Unix-only, matching existing app fixture suites. In Linux library unit-test builds, the normal constructor must return ErrorKind::Unsupported and availability must return UnsupportedHost because native_coordinator is intentionally absent; never select a fixture implicitly. Add an exact unit assertion for this cfg. Production availability/construction tests belong in tests/api_surface.rs against the normal library, both with and without test-support; those normal Linux builds use native_coordinator.

```rust
// cache/native_coordinator.rs
#[cfg(target_os = "linux")]
use super::linux as platform;
#[cfg(windows)]
use super::windows as platform;
#[path = "coordinator.rs"]
pub(crate) mod driver;

// cache/fixture_coordinator.rs
use crate::cache::fixture_platform as platform;
#[path = "coordinator.rs"]
pub(crate) mod driver;
```

Keep `#[cfg(all(test, unix))] mod tests` inside the shared driver source, so its existing private tests are children of the fixture driver. Methods/types those tests previously accessed stay private. Native providers still run their own native suites. Keep NEXT_OPERATION as **one shared static in download.rs**, not one static per compiled driver; zero/wrap refusal remains exact and tokens cannot collide across engines in support builds.

- [ ] **Implement the facade without raw access.** `Client` delegates via its private Engine enum; `open_legacy_imager` selects native provider only or returns Unsupported. Start consumes the opaque selection and feeds its private release to driver start. No constructor takes an app identity, directory, URL, permission bool, selected-manifest data or callback. `verify_local_components` delegates to private bundle verification; the app compatibility module reexports format and this wrapper only.

```rust
impl Client {
    pub fn start(&self, selection: ReleaseSelection) -> Result<Token, Error> {
        self.engine.start(selection.release)
    }
    pub fn resume(&self, selection: ReleaseSelection) -> Result<Token, Error> {
        self.start(selection)
    }
}
pub fn verify_local_components(
    bundle: &crate::format::InternalBundle, path: &std::path::Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), Error> {
    crate::bundle::verify_internal_components(bundle, path, cancel).map_err(Error::failed)
}
```

`Engine::start(InternalRelease) -> Result<Token, Error>` is private dispatch over the driver variants; implement each public control/status/close method using the same exhaustive enum match, no trait object exposing native authority. `Error::failed` is pub(crate), not an app error/proof factory.

- [ ] **Categorize failures structurally at their origins.** Add crate-private Error constructors `invalid_input`, `failed`, `busy`, `stale`, `closed`, `unsupported`, `permission_denied`, each taking `impl Into<String>`; an `io(std::io::Error)` helper maps the native kind. Preserve original message strings. Explicit busy lock branches become Busy; filesystem qualification becomes Unsupported; wrong owner/mode/ACL becomes PermissionDenied; stale-token operations become Stale. Change private Result aliases/provider signatures and their test implementations coherently; update assertions to inspect message/kind without weakening them. `finish_held_read` still executes and concatenates every check, with uncertainty on any boundary failure. Keep operation phase as the cancellation/paused outcome; attach the error rather than discarding it. These Result/DTO/phase adaptations are explicit residuals; no checkpoint encoding, hash domain, trust check, native validation or recovery algorithm is rewritten to accommodate them.

- [ ] **Add the semantic surface regression using positive API references.** This is a compile surface test; production-behavior tests remain moved suites. Test normal support-enabled construction separately on each host to ensure no implicit fixture fallback.

```rust
#[test]
fn normal_facade_has_semantic_controls_only() {
    use tilo_install_artifacts::{Client, Error, ReleaseSelection, Token};
    let _: fn(&Client, ReleaseSelection) -> Result<Token, Error> = Client::start;
    let _: fn(&Client, &Token) -> Result<(), Error> = Client::pause;
    let _: fn(&Client, &Token) -> Result<(), Error> = Client::release;
}
```

- [ ] **Move test helpers behind the exact dev facade.** Move Server from transport tests into src/test_support.rs, retaining private server fields and transport conversion. Declare that module privately under all(test, not(feature = "test-support")) and publicly only under feature = "test-support"; Unix-gate Server and its impl. The public Server exposes only the spec methods. `Server::client(root, preference)` explicitly selects fixture Engine and retains server consumption counts; it does not return InternalTransport or accept consumer callbacks. Fixture implementation can use the preference argument only after Task 2; until then require the exact method signature and retain it in Client fixture state for later storage, without I/O. Move the old app callbacks that asserted “must not consume” to assertions that `consumptions() == 0`; do not drop the assertion.

- [ ] **Migrate all production callers and remove only cache platform gates.** App `InternalRelease` becomes an alias for ReleaseSelection; catalog functions delegate, host comparison uses SDK catalog_architecture. Flow uses Client and public Token/Phase/Status/Progress; production constructor uses the explicitly named legacy entrance. View uses semantic availability/error instead of Linux/Windows cfg around this card/commands/fields/module. Preserve full-OS review guards and unsupported host UI text; do not turn this into install support. Keep preference load/save code and schema unchanged until Task 2. Update bundle compatibility API to forward to the service; delete native app cache/download implementations.

- [ ] **Update exact subprocess filters and prove execution.** New literals are `cache::linux::tests::native::lock_probe` and `cache::windows::tests::native::lock_probe`. Capture child output and assert it includes exactly one executed test and the expected test name before asserting success, and retain the existing child return-value assertions. Preserve `--ignored` only for the already ignored child fixture; do not add new ignores. Count attributes before/after and preserve existing ignore reason text.

- [ ] **Run narrow service and consumer checks.** Native host: `cargo test -p tilo-install-artifacts --lib cache --locked`, `cargo test -p tilo-install-artifacts --lib transport --locked`, `cargo test -p tilo-install-artifacts --test api_surface --locked`; `cargo check -p imager --tests --locked`; `cargo test -p imager --lib internal_download_flow --locked`; `cargo test -p imager --lib internal_download --locked`. Confirm each filter selects nonzero intended tests; if a renamed view test is not included, run its exact name from the moved-test inventory. Run `cargo check -p tilo-install-artifacts --all-targets --locked --features test-support` without treating it as a native behavior pass. Save counts and residue/refusal/cancellation test identities.

- [ ] **Freeze the complete Task 1 for independent specification and code review.** Report normalized-equal moves, explicit residual changes, moved tests, crate graph and current app native exceptions. Parent may accept the ownership milestone while Task 2 still fixes preference persistence, but may not mark the overall plan complete.

### Task 2: Preference persistence through atomic-file with typed uncertainty

**Files:**

- Create `crates/packaging/install-artifacts/src/preference.rs`; modify service `download.rs`, `cache/coordinator.rs`, `error.rs`, `lib.rs`, `test_support.rs`, Cargo manifest; add atomic-file normal dependency.
- Modify SDK `src/install_artifacts.rs` public exports; app `setup/internal_download_flow.rs` and its existing embedded tests; app `setup/view.rs` tests/status strings only where preference loading/warnings require it.
- Create service `tests/preference_publication.rs`; add the design’s exact client_with_preference_fault/client_with_preference_hold methods to service test_support without exposing generic storage operations. Enable atomic-file/test-support through the service test-support feature and its dev dependency only.

**Interfaces:**

- Consumes existing `atomic_file::{provision_directory, StorageDirectory, Privacy, PublishMode, PublicationState, DirectoryState}` on Unix and existing `write_creating_parent_with_mode`/`publication_state` on Windows.
- Produces **final** `Client::start(&self, ReleaseSelection, Vec<u8>) -> Result<Token, Error>`, `Client::preference() -> PreferenceState` and the design's exact preference enums/records. `resume` keeps its final one-selection signature and does not save.
- Private storage signatures: `PreferenceStore::load(&self) -> Result<Option<Vec<u8>>, PreferenceFailure>` and `save(&self, &[u8]) -> Result<PreferenceCommit, PreferenceFailure>`. Private store path is fixed in production, supplied only by dev fixture factory.

- [ ] **Write failing production-path persistence tests.** Reuse atomic-file fault hook capabilities instead of writing a fake filesystem. Tests must cover zero bytes/4096 accepted, 4097 rejected before effects, absent/oversized/malformed old preference, read-only no-create, pre-publication failure, post-publication flush failure, and later independent reread. On Unix test the actual preference store under an isolated absolute user-owned root. On Windows assert NamespaceUnproven on successful wrapper publication, never Durable. Failure injection must target the production storage call and fail if its checkpoint was not observed.

```rust
// Private unit test in preference.rs: tests see private at() only.
#[test]
fn size_refusal_has_no_storage_effect() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing/selection.json");
    let store = PreferenceStore::at(path.clone());
    let error = store.save(&vec![b'x'; 4097]).unwrap_err();
    assert_eq!(error.publication, PreferencePublication::NotPublished);
    assert_eq!(error.namespace, PreferenceNamespace::Unchanged);
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn load_of_absent_preference_never_creates_parents() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing/selection.json");
    let store = PreferenceStore::at(path.clone());
    assert_eq!(store.load().unwrap(), None);
    assert!(!path.parent().unwrap().exists());
}
```

- [ ] **Run red tests against the existing behavior first.** `cargo test -p tilo-install-artifacts --lib preference --locked` must select the added tests. Use the moved legacy writer as the initial private store implementation, run the actual injected checkpoint/phase tests, and require a failure because the production checkpoint was not reached or the old writer incorrectly reports success. Missing symbols/packages and unrelated environment failures are not behavioral RED evidence. Do not fill a host volume or require elevated credentials.

- [ ] **Implement bounded no-follow load and exact native save mapping.** Use a File only privately and `take(4097)` after metadata bounds; reject unsupported data root before creation. The app still decodes SelectionKey and maps malformed JSON to no saved key. For Unix, use held namespace control and real atomic-file publication; all errors return both namespace and publication knowledge. Never remove the published path to compensate for a flush error.

```rust
// Unix save body after byte/path validation; mapping helpers are private.
let parent = self.path.parent().ok_or_else(invalid_preference_path)?;
let name = self.path.file_name().ok_or_else(invalid_preference_path)?;
let directory = atomic_file::provision_directory(parent, atomic_file::Privacy::Inherit)
    .map_err(map_provision_failure)?;
directory.validate_namespace_control().map_err(map_admission_failure)?;
directory.publish(name, bytes, atomic_file::PublishMode::Replace, atomic_file::Privacy::OwnerOnly)
    .map_err(map_publication_failure)?;
Ok(PreferenceCommit { durability: PreferenceDurability::Durable })
```

Define `invalid_preference_path() -> PreferenceFailure` with InvalidInput/NotPublished/Unchanged; `map_provision_failure(ProvisionError)` maps Unchanged/CreatedDurabilityUnknown/NamespaceUnknown verbatim and NotPublished; `map_admission_failure(io::Error)` reports NotPublished/Durable (the admitted/provisioned chain completed); `map_publication_failure(PublicationError)` maps its exact publication state and Durable namespace. Native error-kind classification uses the Task 1 helper, not string matching. Windows calls the current existing wrapper with constant 0o600; any successful call returns NamespaceUnproven; map publication_state when present, otherwise NotPublished, and namespace Unproven. No new native Windows directory durability protocol belongs in this task.

- [ ] **Integrate load/save into the existing owned worker, not UI callbacks.** Add private `Request.saved_key: Option<Vec<u8>>`; start supplies Some, resume/discard None. Worker startup reads once and publishes PreferenceState. Store it in the existing bounded/coalesced mailbox, not an unbounded message stream. Add SavingPreference phase. Save before authentication/cache acquisition; failed save terminates without network/cache effects, preserves typed PreferenceFailure, and sets uncertain according to possible publication/namespace effects. On cancellation during save retain its outcome but do not proceed. PreferenceCommit is retained in all later statuses for the token.

```rust
// Worker ordering; the existing stop/report functions remain the owners.
if let Some(bytes) = request.saved_key.as_deref() {
    stop(&request.flags)?;
    status.phase = Phase::SavingPreference;
    report(shared, status);
    let commit = preference.save(bytes).map_err(|failure| {
        publish_preference_failure(shared, &failure);
        preference_operation_error(failure)
    })?;
    status.preference_commit = Some(commit);
    report(shared, status);
    stop(&request.flags)?;
}
```

`publish_preference_failure(&Shared, &PreferenceFailure)` updates the existing bounded PreferenceState; `preference_operation_error(PreferenceFailure) -> Error` retains all facts in the mailbox and sets uncertain if publication is not NotPublished or namespace is unknown/unproven/created-undurable. The worker owns PreferenceStore. Do not accept a request for a stale selection simply because a preference exists.

- [ ] **Update the flow while preserving app representation and policy.** Remove std::fs/File/tempfile/dirs I/O from SelectionPreference. Keep SelectionKey exactly unchanged. On client startup poll preference; disable commands until load settles. Decode bytes in the app. `start_selected` encodes the key then calls final start(selection, bytes), keeping a pending `(Token, SelectionKey)` until PreferenceCommit appears; do not update saved on admission. `resume` re-resolves saved against current catalog then calls resume(selection). Show durable-namespace uncertainty as a retained warning when NamespaceUnproven; keep save errors visible. Existing review conditions and installation refusal remain.

```rust
// Changed positive API surface test after Task 2.
let _: fn(&Client, ReleaseSelection, Vec<u8>) -> Result<Token, Error> = Client::start;
let _: fn(&Client, ReleaseSelection) -> Result<Token, Error> = Client::resume;
let _: fn(&Client) -> PreferenceState = Client::preference;
```

- [ ] **Install worker-local fault observation through the exact fixture methods.** `PreferenceFault::{BeforePublish,BeforeCommit}` maps to the existing atomic-file checkpoint with the same name. `Server::client_with_preference_fault(root, preference, fault)` returns `(Client, Receiver<PreferenceFault>)`; send the checkpoint acknowledgement from an atomic-file observer installed on the worker, and nest `with_failure(checkpoint, || store.save(bytes))` in that same worker scope. `Server::client_with_preference_hold(root, preference)` returns `(Client, Receiver<()>, Sender<()>)`; install a worker observer at BeforeCommit, send `()`, then require the release channel within ten seconds. Both wrap the actual private PreferenceStore::save. The existing hooks are thread-local and a hook installed on the caller thread cannot prove an async worker failure. The fixture’s private configuration is carried into the worker on creation; the ordinary constructor carries none. Test code uses recv_timeout and asserts receipt before cancel/release. A timeout is a failed test, never skipped coverage.

```rust
// Unix private preference unit test: known publication, failed directory commit.
#[test]
fn commit_failure_keeps_published_bytes_and_reports_uncertainty() {
    use atomic_file::test_support::{Checkpoint, with_failure, with_observer};
    use std::{cell::Cell, rc::Rc};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("selection.json");
    let store = PreferenceStore::at(path.clone());
    store.save(b"old").unwrap();
    let observed = Rc::new(Cell::new(false));
    let seen = Rc::clone(&observed);
    let result = with_observer(move |event| {
        if event.checkpoint == Checkpoint::BeforeCommit { seen.set(true); }
    }, || with_failure(Checkpoint::BeforeCommit, || store.save(b"new")));
    assert!(observed.get(), "production commit checkpoint must be observed");
    let failure = result.unwrap_err();
    assert_eq!(failure.publication, PreferencePublication::PublishedDurabilityUnknown);
    assert_eq!(failure.namespace, PreferenceNamespace::Durable);
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    assert_eq!(store.load().unwrap().unwrap(), b"new");
}
```

- [ ] **Add ordering/cancellation tests through the real facade.** With Server's localhost requests counter, inject save failure and assert no requests and no cache namespace. Hold at preference flush, cancel, release the hold, assert terminal Cancelled plus retained publication/namespace facts and zero requests. Coalesce past SavingPreference and verify the app still updates saved exactly once from the later token's commit. Changed catalog, duplicate architecture, foreign token and malformed saved JSON must still refuse. In Windows native test, success may download with NamespaceUnproven but must render the warning; no Durable assertion is permitted.

- [ ] **Run targeted checks and freeze independently.** `cargo test -p tilo-install-artifacts --lib preference --locked`, `cargo test -p tilo-install-artifacts --test preference_publication --locked`, `cargo test -p imager --lib internal_download_flow --locked`, `cargo check -p imager --tests --locked`. Confirm filters are nonzero. Use existing BeforePublish/BeforeCommit checkpoints; no atomic-file source modification is required or authorized by this task. Review typed phase mapping and actual production-call injection separately from Task 1 relocation.

### Task 3: Privacy, package wiring and final source milestone evidence

**Files:**

- Modify service `src/lib.rs` API docs; SDK `src/install_artifacts.rs`; `.forgejo/workflows/release-imager.yml`; `tools/app_platform_baseline.json` only exact moved-site entries; no baseline allowances added.
- Create service `tests/privacy_probes.py` and `tests/consumer_probes/{positive.rs,private_cache.rs,private_transport.rs,forged_selection.rs,no_consume.rs,no_backend.rs,normal_no_test_support.rs,support_positive.rs,support_no_handles.rs}`.
- Write public relocation report/review/source manifest named above; update framework execution/evidence register with source-only status and exact remaining gates.

**Interfaces:** Final design API, dependency features, real App SDK and existing boundary checker. No new product operations.

- [ ] **Write and run positive/negative consumer probes using actual Cargo-emitted artifacts.** Build normal service and SDK immediately before their probe group; choose the emitted rmeta and matching hashed rlib, not a mutable top-level alias. Capture rustc JSON diagnostic codes. Run positive control first; a build/runtime load/target/metadata error aborts the group. Negative result must contain the intended privacy code and not E0463.

```rust
// positive.rs
use tilo_install_artifacts::{Client, Error, ReleaseSelection, Status, Token};
fn api(client: &Client, release: ReleaseSelection, bytes: Vec<u8>) -> Result<Token, Error> {
    client.start(release, bytes)
}
fn main() {
    let _ = api as fn(&Client, ReleaseSelection, Vec<u8>) -> Result<Token, Error>;
    let _ = Client::status as fn(&Client) -> Option<Status>;
}
```

```rust
// private_cache.rs -- expect E0603
use tilo_install_artifacts::cache::checkpoint::PreparedPrefix;
fn main() {}
// forged_selection.rs -- expect private-field construction failure E0451
use tilo_install_artifacts::ReleaseSelection;
fn forge() -> ReleaseSelection {
    ReleaseSelection { release: panic!("compile-only private-constructor probe") }
}
fn main() {}
// no_consume.rs -- expect E0599
fn steal(client: &tilo_install_artifacts::Client) { client.consume(); }
fn main() {}
```

The forged constructor is never executed; require E0451 for its private field. A separate field-read probe requires E0616. Implement the equivalent SDK probes (`tilo_app::install_artifacts`) and normal/support feature combinations. `private_transport` imports `transport::SelectedManifest`; `no_backend` imports `cache::linux::CacheRoot`; `normal_no_test_support` imports test_support and expects absence; `support_positive` uses Server::selection/client on Unix and verifies presence; `support_no_handles` tries Server::transport and expects E0599. Separately assert ReleaseSelection has no Deserialize using a minimal serde bound and require the trait-bound error, not linkage failure. Support probes must still fail to construct providers/receipts. Verify feature receipts exclude support in normal artifacts and include it only for dev probes.

- [ ] **Wire moved tests into existing native release lanes.** Add `cargo test -p tilo-install-artifacts --lib --locked`, `--doc`, and SDK docs with `--features install-artifacts` beside existing Imager/folder-service tests in Linux/macOS/Windows lanes; Windows commands preserve the existing target argument. Keep Imager consumer suites. Do not execute signing, package release or privileged native fixtures. Native ignored fixtures retain explicit external qualification owners.

- [ ] **Retire only source-boundary entries actually removed.** Run `python3 tools/check_app_platform_contract.py` and `python3 -m unittest discover -s tools/tests -p test_app_platform_contract.py`. Do not regenerate/expand baseline entries. App compatibility reexports contain no native mechanism; catalog/format metadata may remain portable. Unrelated app allowances remain untouched.

- [ ] **Check all required test targets and headless graphs.** Run `cargo check -p tilo-install-artifacts --all-targets --locked` and `cargo check -p imager --tests --locked` natively; cross-check aarch64-unknown-linux-gnu and x86_64-pc-windows-msvc when installed, recording compile-only results. `cargo tree -p tilo-install-artifacts --edges normal` must contain no app/UI/winit/vello edge. Inspect target-resolved graphs/feature receipts; source cfg inspection alone is insufficient. Finish with parent-owned `cargo check -p shell --tests --locked` after UI source holds are released. Missing toolchain/native library blocks the relevant check, not a pass.

- [ ] **Verify exact source and compatibility evidence.** Compare pre/post record encoding fixtures and source constants; all old app cache/download paths must be absent, bundle path only narrow forwarders. Check all 101/26/49 moved test attributes and five existing ignored reasons. Prove catalog bytes unchanged and attribute folder-service inputs to their baseline/authorized peer receipt, with zero extraction-owned edits there; Cargo.lock changes are dependency edges/new workspace package only. Demonstrate fixed catalog path in packaged host compilation input; this is not package signing/native installed execution evidence.

- [ ] **Freeze and obtain independent SDD specification/code review.** Public report includes source hashes, normalized/residual relocation diff identities, direct probes and expected diagnostics, normal/dev feature trees, filtered test counts, actual native-vs-fixture-vs-cross results, preference phase evidence and unresolved program gates. Any changed frozen source reopens affected checks. Parent owns final integration and commits; no comprehensive CI/support claim is made from source relocation alone.

## Required follow-on work outside this executable source milestone

| Gate / owner | Concrete dependency and acceptance |
| --- | --- |
| G02 host integration / UI owner | Imager must use the component host path; add `HostContext::install_artifacts(&self) -> Result<Client, Error>` only after a real authenticated invocation/revocation contract is available. Bind existing package/workspace/device/instance at prepare, signal close before root drop, use existing WakeHandle for delivery only, retain thread/native leases until quiescent. Remove `open_legacy_imager` compatibility and verify per-invoker native/publication rechecks. No Send permission snapshot or author-derived authority is invented here. |
| S01 private native storage owner | Compare cache admission/snapshots/retirement/final rename against atomic-file, add equivalent shared operations there, then migrate consumers under native crash/race/privacy tests. Preserve stricter cache profile admission. Folder-service common-native work waits for its own formal handoff. |
| S02 lease owner | Integrate FileLease without duplicated owning lock handles; prove Windows byte-range/share and Linux flock interoperability, lock-file identity/security checks and process-death release before deleting private native lock code. |
| N02 macOS backend owner | Implement real private cache admission, retained ancestry, no-replace publication, namespace barriers, restart and held consumption on qualified filesystems; run native same-user/race/ACL/cancellation/disk-full qualification. Fixture tests/UnsupportedHost do not satisfy this deliverable. |
| S01 Windows namespace owner | Provide qualified ancestor/entry persistence and admitted publication profile support before returning Durable preferences or durable cache acknowledgements. Include NTFS/ReFS decisions and actual crash/native evidence; do not silently narrow required host behavior in a generic facade. |
| L02 location owner | Retain old cache/preference discovery until explicit per-user/package/workspace migration with one concurrent-upgrade winner and restart tests. No temp fallback; schema/root changes are separate migration work. |
| L03 / distribution owners | Replace compile-architecture compatibility with native host facts including emulation; qualify real signed/installed artifacts and native tests for each claimed OS/architecture. Existing catalog provenance must survive shipping. |

These gates are part of the already authorized overall goal and are not optional polish. The source plan finishes when its three task artifacts and checks pass; the broader migration remains open until the listed native/authority/location evidence is complete.

## Plan self-review

Coverage: catalog/trust/portable format and sealed cache/transport/coordinator/native verifier with caller migration are the single cohesive Task 1; existing-storage preference fix with async ordering and typed uncertainty is Task 2; privacy/build/package/boundary evidence is Task 3. HostContext facts and seven outstanding program dependencies have explicit owners and acceptance above. No app fake capability, raw-media activation, folder-service visibility change, format migration or broad backend replacement is hidden in the plan. Parent source handoff is required; no further routine design approval is requested.
