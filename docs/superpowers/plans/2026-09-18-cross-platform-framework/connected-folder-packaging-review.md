### Spec Compliance

- ❌ Issues found in the bounded eight-file packaging increment: `dist/verify-payload.sh:57` does not always supply the absolute executable path required by `dist/ci/verify-installed-imager.py:20`; see Important finding below. All eight expected product files are represented in the snapshot-relative diff. The unchanged release workflow intentionally reuses the accepted Task 2 edits (`.forgejo/workflows/release-imager.yml:368`, `:514`, `:661`).
- ⚠️ Full Task 3 qualification is not approved by this review: native Linux/Windows suites, full Linux payload harness, real installed FHS/Nix startup, installed production leases, NTFS modes, and the parent's final `cargo check -p shell --tests --locked` remain open, as recorded in `task-3-report.md:64`. Portable fixture success does not establish any of those results.
- ⚠️ The mechanical extraction, original test preservation, and prior privacy/boundary probes belong to the accepted Task 2 review. This bounded diff does not independently re-prove them.

### Strengths

- `dist/ci/verify-installed-imager.py:23` centralizes exact exit 2/5-second receiver and exit 0/10-second package-smoke assertions. `:28` drops inherited descriptors, closes fd 3, captures diagnostics, and runs the absolute executable from `/`; failures remain failures.
- `dist/ci/run-payload-contract-tests.sh:23` applies descriptor and negative-exit/timeout controls through both existing layout owners. `:49` requires the intended diagnostic, preventing an unrelated ELF/library failure from falsely satisfying a negative control. The diff retains the existing ELF/architecture/library checks.
- `dist/ci/test-installed-imager.py:24` compiles and invokes a real C child; `dist/ci/fixtures/import-smoke.c:7` observes inherited descriptors. Verified green log records nine passes without warning output; the verified mutation log shows descriptor leakage is detected rather than the test passing tautologically.
- `dist/ci/install-build-deps.sh:15`, `:25`, `:35`, and `dist/ci/run-package-job.sh:452` provide Python explicitly in the test environments without adding a shipped application runtime.

### Issues

#### Critical (Must Fix)

- None found.

#### Important (Should Fix)

- **P2 — Preserve relative staging roots:** `dist/verify-payload.sh:57` passes `$(path /usr/bin/tilo-import-folders)` directly to the new helper, but `root=${1%/}` at `:6` retains a relative ROOT. `dist/ci/verify-installed-imager.py:20` rejects that path before testing the executable. This is reachable through the existing Debian packaging interface: `dist/debian/build-deb.sh:7` accepts `TILO_PACKAGE_OUT=out`, derives the relative `main` at `:24`, and invokes this verifier at `:91`. Previously its direct executable invocation worked relative to the current directory; it now fails with usage exit 2. Canonicalize the existing FHS root before deriving the executable, as the Nix owner already does, and add a focused relative-root regression through the FHS owner. No executable search or fallback is needed.

#### Minor (Nice to Have)

- **P3 — Correct syntax-check evidence:** `task-3-report.md:40` and `task-3-validation-results.json:4` record one `bash -n` invocation with five script arguments. Bash parses only the first script; the remaining names become positional arguments. I ran a separate `bash -n` for each of the five changed shell scripts; all returned 0 without stderr. Update the report/receipt to distinguish these actual checks from the original incomplete command. This is an evidence correction, not a remaining parse failure.
- **P3 — Carry reused warning evidence explicitly:** `task3-macos-service-lib.log:461` reports 50 compiler warnings, including unused imports at `:2`; `task-3-report.md:58` summarizes the reused run without mentioning them. Its 313/0/3 result at log `:789` is valid, but not pristine. Record the warnings in the handoff and retain the Task 2 owner's disposition; this packaging task should not silently edit frozen Rust sources to suppress them.

### Checks and Scope

- Read `task-3-source.diff` once as the complete eight-file snapshot-relative change, including new bodies. Used the parent's independent current/archive/diff and unchanged workflow/lock hash verification; did not repeat git or archive work.
- Independently verified all three smoke log SHA-256 values and that the smoke receipt's eight source hashes exactly equal `task-3-after.json`. Read the full RED (three expected failures), GREEN (nine passes), and descriptor mutation (expected leakage failure) logs. Did not rerun those tests.
- Independently rehashed all 55 native macOS receipt sources and six actual CI-selector inputs; all matched. Located the native log by its recorded SHA-256 and verified its result and warnings. Verified both selector output hashes and inspected actual selections: service change selects 62 including service/SDK/Imager; root manifest selects 146/full workspace.
- Inspected `task-3-service-tree.txt`; no normal graph node named ui, winit, vello, wgpu, tilo-app, imager, or qos-ntfs-helper appeared. This is graph evidence only.
- Named risk: absolute-only helper may reject an existing package root. Focused reads of the FHS/Nix path initializers and verifier call sites identified the relative Debian-output regression above. No native algorithm inspection was needed.
- Named risk: new Python/compiler prerequisites may be missing from an owner. Focused call-site inspection confirmed distro provisioning, Nix runtime wrapping, and the existing package preflight's Python/compiler requirements (`.forgejo/workflows/package-desktop.yml:79`).
- Named risk: workflow commands may not cover each host. Focused search confirmed service library/docs and SDK folder-copy docs beside existing Imager commands in all three release lanes, preserving the explicit Windows target.
- Focused verification only: separate `bash -n` calls for `dist/verify-payload.sh`, `dist/ci/verify-nix-result.sh`, `dist/ci/run-payload-contract-tests.sh`, `dist/ci/install-build-deps.sh`, and `dist/ci/run-package-job.sh` all exited 0. No suite/build, package installation, release action, product edit, staging, commit, or subagent was performed.

### Assessment

**Task quality: Needs fixes.**

**Reasoning:** The shared helper and real-process failure controls are sound for the bounded startup contract, with explicit environmental limits. Fix the FHS relative-root regression before accepting this source increment; native/distribution completion remains independently gated afterward.
