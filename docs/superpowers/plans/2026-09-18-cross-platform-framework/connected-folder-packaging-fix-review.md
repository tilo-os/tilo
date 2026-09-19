### Finding Verdicts

- **P2 — Preserve relative staging roots** — **ADDRESSED.** `dist/verify-payload.sh:6-10` now resolves the supplied existing staging root with `CDPATH= cd -- "$root" && pwd -P`, so `path()` passes an absolute installed executable to the unchanged absolute-only helper. `dist/ci/test-installed-imager.py:88-126` exercises the actual FHS owner with a relative root containing a space and trailing slash and proves that both receiver and package-smoke startup assertions are reached. `dist/ci/run-payload-contract-tests.sh:136-142` also drives the existing FHS contract group through relative `fhs` for the future native Linux harness. The saved RED log records both pre-fix subcases stopping at helper usage exit 2; the saved GREEN log records all 10 post-fix tests passing.
- **P3 — Correct syntax-check evidence** — **ADDRESSED.** `.superpowers/sdd/2026-09-18-connected-folder-extraction/task-3-report.md:42` and `task-3-validation-results.json:1-15` explicitly state that the original five-name invocation parsed only `dist/verify-payload.sh`. `task-3-fix-1-validation.json:2-60` records separate exit-0 checks for all five scripts, distinguishes the two post-fix writer executions from the three unchanged checks reused from the independent review, and binds each result to the reviewed source hash.
- **P3 — Carry reused warning evidence explicitly** — **ADDRESSED.** `.superpowers/sdd/2026-09-18-connected-folder-extraction/task-3-report.md:60` and `task-3-fix-1-report.md:24-28` disclose the reused run as 313 passed, 0 failed, 3 ignored, with 50 compiler warnings and the Task 2 nonblocking disposition. `task3-macos-service-lib.log:461` records the 50-warning summary and `:789` records the stated test result; its SHA-256 matches `task3-macos-service-evidence.json`.

### New Breakage in the Fix Diff

None. The three-file delta preserves the helper's absolute-only contract and existing ELF, architecture, shared-library, descriptor, exit, timeout, always-zero, and missing-executable controls. Canonicalization fails closed for a missing staging directory, while valid relative, absolute, root, spaced, and trailing-slash inputs resolve before installed-path construction. The new test stops on the intended startup diagnostics before platform-specific ELF inspection, and does not claim Linux package acceptance.

### Out-of-Scope Observations

None newly discovered. Native Linux and Windows service execution, the full Linux payload harness, real installed FHS/Nix production startup, installed production leases, NTFS coordinator modes, the parent-owned final shell check, and the recorded G02/S01/S02/I01/C01/N02/N04 gates remain explicitly open. Portable fixture success does not close any of them.

### Evidence Checks

- The fix diff SHA-256, all three before/after source hashes, and the immutable baseline manifest/diff/snapshot hashes match `task-3-fix-1-manifest.json`.
- All eight current product-source hashes match `task-3-final-smoke-evidence.json` and `task-3-final-after.json`; the refreshed complete source diff, snapshot, unchanged workflow, and unchanged Cargo.lock hashes also match their receipts.
- The RED log hash and contents match the receipt: one focused test, two failing subcases, each receiving helper usage exit 2 instead of the expected startup failure.
- The GREEN log hash and contents match the receipt: 10 tests passed in 15.763 seconds, including the nine retained helper regressions and the new actual-owner relative-root regression.
- No suite, build, package install, native fixture, Git mutation, or product edit was performed for this re-review.

### Verdict

**Fix round: All findings addressed, no new Critical/Important breakage.**

**Spec verdict: PASS for the bounded fixed Task 3 packaging increment.** The FHS owner again supports its existing relative staging-root interface while preserving the installed helper's absolute-path boundary and the package verifier's original controls.

**Quality verdict: APPROVED for the bounded fixed increment.** Full Task 3 and native/distribution qualification remain open as listed above.
