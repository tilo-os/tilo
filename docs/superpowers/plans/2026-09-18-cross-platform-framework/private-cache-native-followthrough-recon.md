# Private-cache native follow-through: source reconnaissance

This is a source-only input for the next implementation plan, not backend design approval or native qualification. Task3 extraction/API/boundary verification is running separately. The overall N02 goal remains incomplete after that source milestone.

## Confirmed production gap

`install-artifacts/src/cache.rs` enables the production native coordinator for Windows and non-test Linux only. `cache/native_coordinator.rs` selects only Linux or Windows providers. There is no macOS production cache provider in this composition. Explicit Unix fixture support is separately selected and cannot satisfy macOS production availability.

## Required closure to design together

| Existing owner | Required behavior to carry into macOS/shared storage work |
| --- | --- |
| `cache/checkpoint.rs::Provider` | Held ownership verification, bounded journal scan, retirement only of validated identities, publication with exact uncertainty and committed namespace; preserve existing record encoding and recovery rejection. |
| `cache/checkpoint.rs::FreshProvider` | Exclusive in-process freshness consumed before selection checks; irreversible preparation revocation/poison; no reopened or decoded record may mint this authority. |
| `cache/linux.rs::CacheRoot` | Retained ancestry, platform filesystem admission, identity/privacy revalidation, owned lock lifetime and bootstrap. Existing Linux fstatfs, ACL handling and flock are platform implementations, not reusable macOS qualification evidence. |
| `cache/linux.rs::Partial` / `FinalHandle` | No-replace final publication, discard only validated ownership, retained residue and held-reader verification with all postchecks. Linux renameat2 cannot simply be enabled under a broader Unix cfg. |
| `atomic-file/src/directory.rs::StorageDirectory` | Existing held identity/privacy/namespace-control/read/lease/existing-commit/publish operations are reuse candidates. They do not by themselves implement the cache's journal/freshness/retirement protocol. |

The next executable plan must first compare required cache operations with shared atomic-file and FileLease APIs. Add missing common operations only where their identity, privacy, no-replace, namespace-commit and uncertainty contracts match. Preserve stricter cache profile admission; do not replace a native provider with the fixture implementation or broaden existing Linux cfgs mechanically.

## Acceptance that remains required

- Real macOS production construction and owned-worker operation against qualified native storage; same app semantic API without app backend selection.
- Exact record/catalog compatibility, resume/restart, private ACL/ancestry and identity/race checks, no-replace final publication, held consumption, cancellation and cleanup ownership.
- Native fault/crash, full-volume and permission cases using appropriate isolated fixtures; missing observation must remain a failed/open gate.
- Windows namespace/profile completion, native Linux/Windows conformance, G02 authenticated invocation/revocation, L02 location compatibility and installed-package tests remain separate dependencies. Implementing macOS alone cannot close them.

No product files, Cargo dependencies, native fixtures or machine configuration were changed by this reconnaissance. OS primitive choices need a separate source/documentation review and executable plan before implementation.

## Source identities at reconnaissance

- `crates/packaging/install-artifacts/src/cache.rs`: `86910bbd5618fccdd80d535d0c17af25ac1894e30852b71012e387333eacd67a`
- `crates/packaging/install-artifacts/src/cache/native_coordinator.rs`: `2f62dfc398f8c21642838ab7707860ce151199e624a6f4fe012fa86487e28578`
- `crates/packaging/install-artifacts/src/cache/checkpoint.rs`: `73800f770f79431b8f771e7c1533a4c7f393567a53daa57dfa029dc991095c73`
- `crates/packaging/install-artifacts/src/cache/linux.rs`: `8d4fa08684143c2210875b1093eec5a12de35ac1e54094fb0e1bbbe747341fed`
- `crates/system/atomic-file/src/directory.rs`: `fa01cd05b1095041e6733d2536b9fb41425e6f72a4b26d887719f8dd81dbccbb`
