# Task 3 fix round 1 — frozen for scoped rereview

Addressed the Important relative-FHS-root regression and both evidence minors from `task-3-review.md`. This round changes exactly three existing files inside the original eight-file product scope. The production helper remains byte-identical, including its absolute-only contract, descriptor sanitization, exact exits, and timeouts. No Rust/service/app/SDK source, workflow, lockfile, install dependency, or unrelated WIP was changed.

## Relative staging root

The actual Debian builder accepts `TILO_PACKAGE_OUT=out`, produces a relative staging path, and passes it to `dist/verify-payload.sh`. The FHS owner previously retained that relative root and passed a relative executable to the new absolute-only helper. The helper correctly rejected it with usage exit 2, preventing ordinary relative package outputs.

`dist/verify-payload.sh` now canonicalizes the existing staging directory using `CDPATH= cd -- "$root" && pwd -P` before deriving paths. This resolves the supplied root without searching for an executable or permitting a PATH fallback. Invalid/missing roots still fail. CDPATH cannot substitute a different relative location.

`dist/ci/test-installed-imager.py` adds one focused test through the **actual FHS owner**. It constructs the expected layout from the real repository assets, installs the same compiler-produced native fixture, and invokes the owner with a relative root containing a space and trailing slash. Two subcases require the real installed receiver's wrong-exit diagnostic and the package-smoke wrong-exit diagnostic after a successful receiver check. These negative controls prove actual path resolution/startup execution on macOS without replacing `file`/`ldd`, bypassing ELF checks, or falsely claiming Linux package acceptance.

`dist/ci/run-payload-contract-tests.sh` now invokes its existing FHS contract group from the fixture directory with relative ROOT `fhs`. A future native Linux run will exercise full positive ELF/architecture/library acceptance plus the unchanged descriptor, wrong-exit, missing-executable, and timeout controls through that relative interface. That full Linux harness was **not run** on this macOS host.

## RED/GREEN and syntax evidence

- RED: `python3 dist/ci/test-installed-imager.py InstalledImagerTests.test_fhs_relative_root_reaches_installed_startup_checks -v` against the pre-fix owner. Exit 1; one test with two failing subcases. Both expected startup exit 1 but got helper usage exit 2. See `task-3-fix-1-red.log`.
- GREEN: `python3 dist/ci/test-installed-imager.py -v` after the fix. Exit 0; **10 tests passed in 15.763 seconds**, including all nine original descriptor/security/exit/timeout tests and the new actual-owner relative-root test. Terminal **38861** completed with exit 0. No live process handles remain. See `task-3-fix-1-green.log` and the current-source-bound `task-3-final-smoke-evidence.json`.
- `bash -n dist/verify-payload.sh` and `bash -n dist/ci/run-payload-contract-tests.sh` were each run separately after their edits: both exit 0, no stderr. The other three shell files are byte-identical to their reviewed hashes; the independent reviewer's individual exit-0 syntax checks are explicitly reused. `task-3-fix-1-validation.json` distinguishes writer execution from reviewer reuse.
- `git diff --check -- dist/verify-payload.sh dist/ci/run-payload-contract-tests.sh dist/ci/test-installed-imager.py` returned 0.

The original single `bash -n` invocation with five filenames parsed only the first script. `task-3-report.md` and `task-3-validation-results.json` now explicitly correct that evidence scope; their original bytes are preserved in `task-3-before-fix-1-report.md` and `task-3-before-fix-1-validation-results.json`. No whole-suite/build repetition was used to obscure the correction.

## Reused warnings and limits

The native macOS service receipt remains 313 passed, 0 failed, 3 pre-existing ignored, and **50 compiler warnings**. This is reused Task 2 execution, not a rerun or warning-clean result. As `task-2-review.md` records, non-Linux unused imports/dormant private native code produce warning noise, and unrelated Automerge/UI warnings also remain. Task 2 treats this as nonblocking for the semantics-preserving extraction and assigns focused cleanup separately; exports/privacy must not be weakened to suppress it. This packaging round preserves that disposition and makes no Rust edits.

Native Linux/Windows service execution, full Linux payload harness, real installed FHS/Nix production startup, installed leases, NTFS qualification modes, and broader G02/S01/S02/I01/C01/N02/N04 gates remain outside this locally observed evidence. The parent owns the final shell checkpoint and broader source handoff. No full Task 3 native qualification or full N03/goal completion is claimed.

## Review artifacts and freeze

- Original `task-3-after.json`, `task-3-source.diff`, `task-3-snapshot.tar`, and the pre-fix nine-test receipt/logs are unchanged and remain the round baseline.
- `task-3-fix-1.diff` contains the complete three-file round delta; `task-3-fix-1-manifest.json` records before/after hashes and immutable baseline hashes.
- `task-3-final-after.json`, `task-3-final-source.diff`, and `task-3-final-snapshot.tar` are the refreshed complete eight-file source increment. The original `task-3-handoff-paths.json` still names the exact eight paths.
- `task-3-final-smoke-evidence.json` binds the **post-fix ten-test** result/logs to the final source hashes and identifies the nine-test receipt as pre-fix evidence only.
- All 113 frozen extraction paths/deletion tombstones were reverified unchanged, as were the release workflow and Cargo.lock. No unrelated product changes, package installations, native provisioning, native device operations, release actions, staging, commits, subagents, or Cargo builds were performed.

Source is frozen for scoped rereview. No live terminal handles remain.
