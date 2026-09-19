- **Native providers still return `String` from closures now requiring `Failure`.** — **ADDRESSED.** The Linux journal wrapper now types the closure input as `crate::error::Failure` and returns `e.with_message(message).with_uncertainty()` only after publication may have occurred, while the non-uncertain branch returns the original `e` (`crates/packaging/install-artifacts/src/cache/linux.rs:991`, `:994-997`). The Linux final-publication wrapper applies the same typed preservation when `renamed` is true and otherwise returns `e` unchanged (`crates/packaging/install-artifacts/src/cache/linux.rs:1127`, `:1129-1132`). The Windows journal wrapper has the matching correction (`crates/packaging/install-artifacts/src/cache/windows.rs:1514`, `:1517-1520`). Existing `Failure::with_message` changes only the diagnostic, and `with_uncertainty` retains the structural `ErrorKind`; the added characterization test covers the four native I/O categories, both incoming uncertainty states, and both exact context prefixes (`crates/packaging/install-artifacts/src/error.rs:77`). Native Linux and Windows service compilation/runtime remain unverified because the available cross environments stop in native dependencies before this crate; the passing host helper test is supporting semantic evidence, not native-backend compilation proof.
- **The moved common test hooks are accidentally absent on Windows.** — **ADDRESSED.** `fixture_storage` is now compiled for every unit-test target and remains available for Unix `test-support` builds through `#[cfg(any(test, all(unix, feature = "test-support")))]` (`crates/packaging/install-artifacts/src/cache.rs:20`). This restores the unchanged `cache::tests` re-export used by the `checkpoint.rs` `cfg(test)` fault/read-event calls on Windows. The actual file-identity, `DiskProvider`, `FreshDisk`, preparation hook, and fixture protocol implementations retain their existing `#[cfg(unix)]` gates (`crates/packaging/install-artifacts/src/cache/fixture_storage.rs:41`, `:53`, `:97`, `:194`, `:219`, `:239`, `:295`, `:351`), so the fix neither exposes Unix fixture providers on Windows nor widens the public fixture API. Windows all-target compilation and native execution remain unverified for the same dependency/toolchain limitation.
- **Remove the unused migrated `Worker` alias.** — **ADDRESSED.** The test import now retains only `Phase`; no production caller or test body changes (`apps/imager/src/setup/internal_download_flow.rs:749`).

### New Breakage in the Fix Diff

None found. The added error test exercises the existing private adapter without changing production behavior, the module-gate change keeps providers Unix-only, and the import edit is local to a Unix-gated test module.

### Out-of-Scope Observations

None.

### Checks

- Reviewed the 107-line fix-only patch against `task-1-fix-1-before/` and the immutable reviewed snapshot; no product, Git, index, manifest, or lockfile changes were made by this review.
- Accepted the verified covering evidence without rerunning it: `error::tests` passed 2 tests, `cache` passed 60 tests, and the host `--all-targets --features test-support` check exited 0. The recorded 12 test warnings and 21 library plus 10 library-test warnings remain visible; no warning-free claim is made.
- No fresh app/shell check was required for the one-line unused test-import removal. Earlier app integration evidence remains a distinct snapshot and is not presented as fix-snapshot execution.

### Scoped Spec Verdict

**PASS.** The fix implements the requested typed native error preservation, restores portable unit-test hooks without exposing native fixture providers or changing public API, and removes the reported unused alias. Native Linux/Windows qualification remains an explicit environment-dependent follow-up rather than evidence supplied by this fix round.

### Scoped Quality Verdict

**PASS.** The fix is minimal and localized, preserves exact diagnostics, structural error category, and uncertainty, and introduces no new Critical, Important, or Minor issue in the fix diff.

### Verdict

**Fix round: All findings addressed, no new Critical/Important breakage.** No findings remain open in this scoped re-review; native Linux/Windows compilation and runtime qualification remain unverified.
