# Task 3 packaging implementation — fix round 1 frozen for review; native qualification open

The current source snapshot and post-fix 10-test receipt are `task-3-final-after.json`, `task-3-final-source.diff`, `task-3-final-snapshot.tar`, and `task-3-final-smoke-evidence.json`. See `task-3-fix-1-report.md` for the relative-root correction and scoped RED/GREEN evidence. Original Task 3 after/diff/snapshot and nine-test logs remain immutable pre-fix evidence.

## Disposition and scope

The bounded package-verifier implementation is complete and its portable subprocess regressions pass. Full Task 3 native qualification, full N03, and the overall extraction goal are **not complete**. No authorized Linux/Windows/package fixture runner is available on this macOS arm64 host. No package installation, release, push, signing, ignored native test, NTFS coordinator, native device operation, broad shell check, staging, commit, stash, Cargo.lock regeneration, or CARGO_TARGET_DIR change was performed.

The relocated service/app/SDK sources remained frozen: all 113 entries, including deleted-source tombstones, in `relocation-after.json` match at the end of this task. All 55 native macOS receipt source hashes and the native test log hash match. The existing six-file Task 3 baseline was checked before editing; `.forgejo/workflows/release-imager.yml` remained byte-identical. No relocation or extraction defect was found or edited. Unrelated shared WIP is outside this increment.

Changed existing files:

- `dist/verify-payload.sh` and `dist/ci/verify-nix-result.sh` invoke the same shared startup verifier with their actual installed executable paths.
- `dist/ci/run-payload-contract-tests.sh` installs a receiver-aware native executable for Imager and exercises both layout owners with descriptor, wrong-exit, timeout, always-zero binary, and missing-executable controls.
- `dist/ci/install-build-deps.sh` explicitly supplies Python as a test prerequisite in Debian/Fedora (`python3`) and Arch (`python`).
- `dist/ci/run-package-job.sh` runs the Nix verifier inside `nix shell nixpkgs#python3`; Python is outside the shipped closure.

Narrowly necessary added files:

- `dist/ci/verify-installed-imager.py`: common production package-test helper.
- `dist/ci/test-installed-imager.py`: portable real-child-process regressions for that helper.
- `dist/ci/fixtures/import-smoke.c`: native fixture shared by the portable tests and existing Linux payload harness; never delivered as an application binary.

The helper accepts only an absolute installed executable path, runs from `/`, supplies stdin from `/dev/null`, captures stdout/stderr, sanitizes inherited descriptors with `close_fds=True`, explicitly closes reserved fd 3 before exec, requires receiver exit **2** within **5 seconds**, then package-smoke exit **0** within **10 seconds**. Wrong exit, signal, spawn failure, or timeout fails verification. It does not locate a checkout/PATH executable. Python's subprocess timeout kills and reaps its child. No lease success is inferred from the invalid-channel check.

The Linux fixture remains a normal compiler-produced executable, dynamically linked under the normal native toolchain. Neither layout verifier's ELF, architecture, library, or Nix closure checks were removed, bypassed, or given fixture exemptions. Both owners challenge the installed fixture with inherited descriptors 3, 9, and 57, then reject actual `true` (receiver exit 0), receiver exit 1, package-smoke exit 2, both hangs, and absence. Negative-control diagnostics must match the startup failure, so an unrelated ELF/ldd failure cannot satisfy those controls. Existing ordinary package/layout/session harness checks are retained.

## Evidence and exact commands

`task-3-after.json` records baseline HEAD, before/after source SHA-256 values, unchanged workflow/Cargo.lock hashes, and patch/snapshot hashes. `task-3-source.diff` is the complete snapshot-relative diff, including all added-file bodies; `task-3-handoff-paths.json` lists exactly eight product files; `task-3-snapshot.tar` carries their actual bytes. These files supplement the parent's full extraction handoff rather than replace it.

1. RED: extracted the previous smoke-only invocation into the real shared helper without adding receiver behavior; ran:

   `python3 dist/ci/test-installed-imager.py InstalledImagerTests.test_rejects_receiver_zero InstalledImagerTests.test_rejects_other_receiver_exit InstalledImagerTests.test_receiver_timeout_fails`

   Exit 1, all three tests failed on `0 == 0`: old behavior accepted each bad receiver. See `task-3-smoke-red.log`.

2. GREEN: `python3 dist/ci/test-installed-imager.py -v` — exit 0, **9 passed**, 15.308 seconds. This includes a native C fixture compiled with `cc -Wall -Wextra -Werror`, exact-exit controls, inherited-descriptor closure, missing executable, PATH refusal, and both timeouts. Terminal **40266** reached terminal exit 0; no process handle remains live. See `task-3-smoke-green.log` and the source/log-bound `task-3-smoke-evidence.json`.

3. Descriptor mutation: copied the unchanged helper/test/fixture into a temporary directory, changed only `close_fds=True` to `False` in that copy, and reran the descriptor regression. It failed as expected because the fixture detected inherited authority and exited 91. The actual helper stayed unchanged. See `task-3-descriptor-mutation.log`.

4. The original multi-file `bash -n` invocation returned 0 but parsed **only `dist/verify-payload.sh`**; the remaining names became positional arguments. Its original receipt is corrected to state this limit. The independent reviewer subsequently ran a separate `bash -n` for each of the five shell scripts; all returned 0. Fix round 1 reran individual syntax checks for its two changed shell scripts and reused the reviewer's checks for the other three unchanged scripts. See `task-3-fix-1-validation.json`.

5. `python3 scripts/check-install-contract.py` — exit 0; contract matches existing Imager/desktop release lanes.

6. `cc -fsyntax-only -Wall -Wextra -Werror dist/ci/fixtures/import-smoke.c` — exit 0.

7. `git diff --check -- dist/verify-payload.sh dist/ci/verify-nix-result.sh dist/ci/run-payload-contract-tests.sh dist/ci/install-build-deps.sh dist/ci/run-package-job.sh` — exit 0.

8. `cargo tree -p tilo-folder-service --edges normal --locked --offline` — exit 0, fresh `task-3-service-tree.txt`; no ui/winit/vello/wgpu/tilo-app/Imager/qos-ntfs-helper graph edge. This is graph inspection, not native execution or a build.

Structured small-check results are in `task-3-validation-results.json`. No unchanged heavy Rust suite was repeated.

## Reused extraction and host evidence

`task-3-reused-evidence.json` confirms the six actual CI-selector input hashes match. The existing `tools/ci_select.py` outputs therefore remain applicable: service-only change selects **62** packages including service, SDK (`tilo-app`), and Imager; root-manifest change selects **146/full workspace**. No second selector graph was introduced.

The existing workflow already has service `--lib`, service `--doc`, and SDK `--doc --features folder-copy` tests beside the existing Imager/action commands in Linux, Windows, and macOS lanes. Windows preserves its explicit target. This task read and reused those edits; it did not dispatch a release job.

The source/log-verified native macOS receipt `task3-macos-service-evidence.json` remains **313 passed, 0 failed, 3 pre-existing ignored**, terminal 70257 previously completed. That run also emitted **50 compiler warnings**, including non-Linux unused imports and dormant private connection code. The Task 2 review treats warning noise as nonblocking for the semantics-preserving extraction and assigns focused cleanup separately, without widening exports or weakening privacy to silence it; unrelated Automerge/UI warnings also remain. No pristine or warning-clean claim is made, and this packaging task made no Rust edits. The reused suite includes the moved macOS ACL, durability, migration, journal, and process-lock tests; the ignored root/image diagnostics were not run. Task 2's service/SDK docs, **25 Imager regressions**, **14 privacy probes**, platform-boundary checker (**55 apps**, **32 checker tests**), and all-target Linux/Windows compile checks are retained as their original evidence, not claimed as rerun. Cross-target checks remain compile-only.

Original deleted sites, all 46 full-file destinations, the scanner/readiness split and callers, exact native subprocess filter changes, semantic API mapping, test names and ignored annotations are preserved in the frozen `task-2-report.md`, `relocation-before.json`, `moved-test-inventory.json`, `split-test-proof.json`, and original test-list logs. All 113 final extraction entries still match their recorded source bytes. The portable packaging fixture does not add production-dispatch or lease evidence.

## Outstanding qualification and execution owners

- **Full Linux harness:** `dist/ci/run-payload-contract-tests.sh` was not executed here. Its ELF/architecture/GNU userspace checks cannot be qualified on macOS, and no Linux runtime exists locally. A provisioned native Linux packaging runner must run the complete harness; portable helper success is not full FHS/Nix harness success.
- **Installed production startup:** both existing owners now contain the required receiver-negative and package-smoke checks, but no real installed Linux/FHS/Nix Imager artifact was exercised in this task. Clean native package lanes must invoke the actual installed executable. This proves early dispatch only; full installed leases remain a separate fixture gate.
- **Linux native service:** run the complete nonignored library suite on its native runner, then separately require both `QOS_ENDPOINT_FIXTURE` and an absolute dispatcher-bearing `QOS_LEASE_HELPER` at shell entry. Inventory nonzero selected test names before running `cargo test -p tilo-folder-service --lib connection::endpoint --locked -- --ignored --nocapture` and `cargo test -p tilo-folder-service --lib connection::transaction::native --locked -- --ignored --nocapture`. Use built `target/debug/examples/early-host` first; repeat the receiver/lease group with the installed Imager executable. Missing fixture, executable, or zero selected tests fails the gate.
- **Windows native service:** on a separately provisioned endpoint root, require `QOS_ENDPOINT_FIXTURE`, inventory cfg-selected nonzero names, then run `cargo test -p tilo-folder-service --lib connection::endpoint::native --locked -- --ignored --nocapture` and `cargo test -p tilo-folder-service --lib connection::store::storage::windows --locked`, including child-process lock tests. The preserved lack of Windows transaction execution is not implementation proof or host completion.
- **NTFS owner qualification:** keep qos-ntfs-helper tests with their owner. No coordinator modes were run. Positive/resume/cancellation/death/crash modes all remain outstanding absent externally provisioned disposable device/client UID and `QOS_NTFS_NATIVE_AUTHORIZATION=DISPOSABLE_FIXTURE_ONLY`. An authorized coordinator run must receive current built probe/Imager paths and the **service** unit-test executable via existing `--native-test`; its authority checks are unchanged.
- **Final repository check:** parent owns the coordinated `cargo check -p shell --tests --locked` checkpoint. This subtask did not duplicate it.
- **Framework gates remain open:** G02 scoped grants/revocation and owner lifetime; S01/S02 equivalent storage/lease consolidation; I01 authenticated transport integration; C01 generalized child ownership; N02 independent cache; N04 readiness service consolidation; all unobserved native/distribution gates above. No full N03 or full-goal completion claim.

Fresh environment inspection records Darwin/arm64, no nix/docker/podman/lima/limactl/colima/system-QEMU executable, and no QOS endpoint/helper/NTFS authorization environment. This is an environmental qualification limit, not a new global source block. The parent owns independent final range review and any ownership transfer to the peer Imager chain. Product edits are frozen for that review; no live terminal/build handles remain.
