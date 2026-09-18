# Connected endpoint evidence — implementation remains incomplete

This audit records the interim native endpoint work for the approved Windows/Linux migration design. It does not enable Connected folders, certify Windows support, or complete the migration goal.

Source commit: `087d5a62c9b94b97489de331919277d02224b0e6`. Its independent source review found four Important defects. Fix commit `0b1f83679` addressed all four; independent scoped re-review accepted them with no new Critical/Important breakage. The defects concern Windows capability promotion, unresolved Linux clone queries, serial namespace requirements, and unsupported Windows metadata. Do not treat the implementation report's original Windows capability claim as correct: the source contradicted it.

## Evidence scope

Native execution used only the disposable Fedora 44 ARM64 fixture, UUID `8fdddf6a-6dae-4f9d-a236-321a1d223e3a`, as ordinary user `parallels`. The host root filesystem is XFS; separate 256 MiB ext4 and Btrfs loop images were created for tests. Frozen source snapshots and logs reside in `.superpowers/sdd/2026-09-16-connected-folders/` and are local, ignored evidence, not distributed test artifacts.

| Native snapshot | Filesystem | Observed result | Limit |
| --- | --- | --- | --- |
| native-red-harness | Btrfs | 1 failed: fresh volume incorrectly ambiguous | Genuine behavioral RED |
| native-probe2-harness | ext4, XFS, Btrfs | 1 identity/observation test passed on each | Predates final source |
| native-probe2-harness | XFS | Selected-write trial failed Unverified | Genuine missing-implementation RED |
| native-proof3-harness | ext4, XFS | 4 tests passed on each | Observation, cancellation/link scope, replaced/offline root, private write trial |
| native-proof3-harness | Btrfs | 4 failed: pseudo-device lookup | Not passing evidence |
| native-proof4-harness | Btrfs | Three test commands returned exit 0 | Full guest logs not retrieved before suspension; empty local copies are not evidence |
| native-proof4-harness | ext4 | Guarded detach/remount coordinator returned exit 0 | Old session unavailable; saved identity reopens in fresh context; guest log pending retrieval |
| native-proof4-harness | Btrfs | Coordinator returned PRL_ERR_IO_STOPPED | Mount-cycle proof absent |

The proof4 outcomes above were observed as command/coordinator results, not subsequently re-read guest logs. They are neither reboot nor power-loss proof. Later source changes and current review fixes require a new native snapshot run.

The Windows actual-backend `cargo +1.97.1 check --tests --target x86_64-pc-windows-msvc --offline` check completed in 4.20 seconds. It compiled real Copy/continuation/endpoint modules with a compile-only portal adapter and omitted UI/TLS. Its ten excluded-UI dead-code warnings were existing harness noise; there were no endpoint warnings. This is compile evidence only. The implementer's 38 host tests and strict host/Linux/Windows source lint results likewise do not substitute for native Windows execution.

Fix verification: 42 host tests passed; strict production lint passed for host, Linux and Windows targets; Linux test compilation passed. The final broader Windows test compilation passed in 1.21 seconds with the same ten existing harness warnings. Windows hard-link, directory-stream and ACL fixtures were added but remain unrun. The frozen `native-fix1-harness` snapshot awaits native execution. None of these cfg checks establishes native support.

A subsequent full-workspace integration check, `cargo +1.97.1 check -p imager --tests --offline`, exited 101 before reaching Imager. Five errors in concurrently modified `ui/src/render_thread.rs` refer to missing `Observer::log_native_config`, `log_native_worker`, and `log_native_present` methods. Retained log: `task-2-fix-1-full-imager-check.log`. The narrow endpoint checks therefore must not be presented as a verified complete app build. Shared UI changes were left intact. A later read-only source check found those methods supplied by the concurrent UI work. The actual host command `cargo +1.97.1 check -p imager --tests --offline` then passed in 30.11 seconds, with one existing `ui::animation_schedule::next_wake` dead-code warning. Retained log: `task-2-native-copy-full-imager-check.log`. This clears that host compile blocker; it is not a Linux/Windows runtime or full target-build result.

## NTFS inspection source candidate — September 17

Commit `b19cb7be0b5ba00013333c29b0ceaa685c04abcf` adds the standalone `qos-ntfs-helper` package. It contains a pinned native NTFS reader, an administrative Linux block-device claim and child-isolation boundary, actual-image fixtures, and install staging. It does not implement mounting, synchronization writes, or Imager/service integration and does not enable Connected capabilities.

The initial candidate's retained run has 17 host native-image tests and one host caller-refusal test passing. All 18 generated fixture images match their recorded full hashes and sizes. Host/Linux focused all-target lint, Linux ARM64 executable/test cross-linking, and Windows all-target compilation passed. The Windows check establishes that the Unix helper does not force Unix build dependencies onto Windows; the helper is unsupported there. The host refusal test reaches the non-Linux branch and does not prove production Linux request validation.

The initial candidate's cross-linked Linux executable has SHA-256 `3cc9e47e2eaf397c44c35b8a0719c100bf28e851742cc33a97bfa12f0b2a4a40`; it has not been executed on Linux. A prior failed cross-build produced an empty archive because host Apple ranlib processed Linux objects. Explicit target ranlib plus positive native-symbol validation corrected that packaging failure. macOS rejects the configured address-space limit, so its uninstalled diagnostic runner explicitly reports the limit unenforced. Linux retains the mandatory limit; host test success is not Linux sandbox evidence.

The initial independent review returned **Needs fixes**, with four required findings:

1. Partition discovery requires a `slaves` sysfs directory that belongs to whole disks, preventing supported partition selection.
2. The pinned NTFS library can select a valid restart page despite another malformed page; the wrapper can incorrectly report clean under the stricter admission contract.
3. The source archive checksum uses a Python assertion that disappears under interpreter optimization.
4. The required runnable detach/replacement invalidation scenario is absent.

Fix commit `122c8dfe011887953325ba5c4bc3ba422448cd2a` addresses all four findings. Independent scoped re-review approved the source changes with no new Critical/Important breakage. The expanded host run passed 21 image tests, one topology regression and one host caller-refusal test; checksum tests cover normal and optimized Python. Final host/Linux lint, Linux binary/test linking and Windows compilation passed. The new production positive and coordinated detach/replacement fixtures are implemented but unrun. Native Linux inventory-to-inspection success, exclusive claims, child isolation, resource limits, replacement, watchdog and automounter cases remain unproved; native Windows session and filesystem behavior also remain unproved. Existing loop fixtures cannot validate the helper's physical-partition inventory route. The native coordinator must use a separately authorized disposable disk presenting supported hardware identity.

The fix build's Linux executable has SHA-256 `a7e24cfdd97904394850a43530c8c42b00ee0fb3f3c8d1fe679aceaff6d92c62`. Its native test entrypoints are cross-linked, and its Python coordinator passes syntax checks; these are not runtime results.

Report, exact diff, review and final logs are retained under the local evidence directory as `task-2-ntfs-stage1-report.md`, `task-2-ntfs-stage1-review.diff`, `task-2-ntfs-stage1-review-report.md`, `task-2-ntfs-stage1-fix-1-review-report.md`, and `task-2-ntfs-stage1-evidence/`. These scoped results do not close the full-app integration blocker or any remaining installation/dual-boot gate.

## Next read-session implementation

A private NTFS3 read-only mount does not establish an immutable raw-read interval. At the inspected Linux source pin `238650ef6c7c7cca08e032527329424c9fbd70e5`, `FSCONFIG_CMD_CREATE_EXCL` prevents adopting an existing matching superblock, but does not prevent a later mount from sharing and reconfiguring that superblock. Private mount propagation and read-only mount flags do not prevent the shared superblock becoming writable. The inspected NTFS3 operations also lack the callbacks required by `FIFREEZE`. This is source evidence about that pin, not a certification of an installed kernel. Primary references: [filesystem creation](https://github.com/torvalds/linux/blob/238650ef6c7c7cca08e032527329424c9fbd70e5/fs/fsopen.c), [superblock reuse/reconfiguration](https://github.com/torvalds/linux/blob/238650ef6c7c7cca08e032527329424c9fbd70e5/fs/super.c), [NTFS3 operations](https://github.com/torvalds/linux/blob/238650ef6c7c7cca08e032527329424c9fbd70e5/fs/ntfs3/super.c), [freeze ioctl](https://github.com/torvalds/linux/blob/238650ef6c7c7cca08e032527329424c9fbd70e5/fs/ioctl.c).

The next source stage therefore retains the exclusive block-device descriptor throughout an administratively authorized read session and performs no mount or freeze. Source commits `1f8e7467ace4bae093adbc78caa7a2668fbbf223` and `7b731a8706bbe75eb8cba1d031235a6a76811d9b` implement metadata-only native manifests, typed completeness validation, bounded 1 MiB content requests, authenticated local IPC and a context-owned endpoint read adapter. Independent review required fixes for nonroot directory access, omitted standalone protocol sources, and late app cancellation. Fix commit `e5e4d3cb66fdc662831280051f4abc48e8658c66` addresses all three; fresh scoped re-review approved the fixes with no new Critical/Important findings. This is source acceptance only. The existing observation hashing budget remains explicit; this stage does not establish arbitrary-folder capacity, metadata round-trip fidelity, Copy UI integration or two-way writes.

Stage 2 diagnostic verification passed 12 native-image cases, including a 12 MiB file with distinct chunks and independent full hash, unaligned ranges, short-name aliases, missing completion records and explicit metadata limitations. The 21 retained Stage 1 image cases, two wire tests and host caller-refusal case also passed. Five helper unit tests cover state and teardown primitives; the interrupted-reap test injects syscall outcomes and is not native signal evidence. The verifier refactor was followed by another passing 12-case run. Root independently checked all 25 final fixture-image hashes against the committed receipt. Host/Linux strict helper/protocol lint, Linux cross-linking and Windows compilation passed. The exact-source endpoint harness passed 42 host connection tests and Linux/Windows compile checks; its whole-harness strict lint remains blocked by excluded UI dead code and two unchanged endpoint-test findings. The subsequent full host Imager check described above passes; full Linux/Windows target builds and native execution remain separate gates.

The native coordinator now exposes 18 opt-in modes covering actual access, ranges, transport, aliases, lifecycle, cancellation, stalled clients and device changes. All remain **UNRUN**, as does the actual Imager native endpoint fixture. No VM, mount, installation or account change occurred. Public read capability remains unverified and write/durability capabilities remain refused. The report and exact owned diff are retained as `task-2-ntfs-stage2-report.md` and `task-2-ntfs-stage2-review.diff`; the latter has SHA-256 `02448b669f074d0f5233b0ea4972a94029f8999360b6a2eb4064851f7916bb6f` and excludes intervening peer changes.

The fix bundle passed actual extracted offline host and Linux cross-builds, including both standalone crates. Linux-only O_PATH pinning preserves the root-owned 0711 directory and authentication checks. The app now checks cancellation after blocking reads/postflight and immediately before returning a tree. Actual nonroot permission and last-chunk/postflight timing fixtures are implemented and cross-linked but unrun. Their single-file selected subtree was verified with real image bytes; the updated 12 MiB regression also passed. All 25 regenerated fixture images match their committed receipt. Fix report and review: `task-2-ntfs-stage2-report.md` (appended fix round) and `task-2-ntfs-stage2-fix-1-review-report.md`; exact fix diff SHA-256 `e42d13a8df1a7450310283c332b3f426f90519d8be3bfd8235c17da4bcd234e4`.

The exact implementation contract is retained locally in `task-2-ntfs-stage2-design.md` and `task-2-ntfs-stage2-brief.md`. No runtime or public capability claim follows from this design. The later write-session authority, atomicity, durability and Windows/Linux metadata preservation requirements remain unresolved delivery work.

## Outstanding delivery gates

- The original endpoint adapter findings 1–4 are fixed and independently re-reviewed. The newer NTFS helper findings above are also source-fixed and independently re-reviewed; native acceptance remains open, and full Task 2 is unaccepted.
- Re-run final source on ext4, XFS and Btrfs, retaining complete outputs.
- Execute read-only, duplicate-volume, mount replacement, reconnect, concurrent-writer and resource-limit fixtures. Prepared controller scripts have only local syntax checks.
- Establish the reviewed NTFS helper's native runtime guarantees and complete Linux-side access to retained Windows folders. Mount/transaction lifecycle and integration remain missing implementation, not merely missing tests.
- Execute Windows native NTFS cases, including locks, placeholders, reparse points, hard links, directory streams and ACL policies; establish actual write/persistence support.
- Complete durable connection storage, transaction execution, conflict/recovery behavior and shipped UI/service integration with native evidence. A final observation sweep is not an atomic filesystem snapshot; transaction-time preimage validation remains required.
- Complete separate Desktop installed lifecycle and Full OS internal staging/dual-boot gates. Production catalogs remain disabled pending authentic signed artifacts and supported native configurations.

Both Fedora VMs were authoritatively listed as suspended during this audit. No resume, UI input, package installation or session changes were performed. Native testing is paused; the original Fedora VM is outside fixture mutation scope.

## Retained local evidence fingerprints

These hashes identify the locally retained artifacts; they do not imply the files are committed or prove unexecuted scenarios.

| Artifact under the local evidence directory | SHA-256 |
| --- | --- |
| `task-2-btrfs-red.log` | `33a47968bab9b2dbb90359bd8654f27c593a3c4e4dc39113d9908dd074c60ece` |
| `task-2-ext4-probe2.log` | `36bca68f443a87d82fe66c298300499c312966fe1cba534b3f44110917b61c3b` |
| `task-2-xfs-probe2.log` | `951b8c166a467ebaf304e506def6ba2850bcbf0514d4217b076cc58434330c8e` |
| `task-2-btrfs-probe2.log` | `2f7e1bcf76bd430cfab6cce91227ad03312e6772911bb71795571d4372538a84` |
| `task-2-write-red.log` | `f458f991b9398859834a1c93ba2b307a37fe1d27bd985dc6037fcaa7d5fd1414` |
| `task-2-ext4-proof3.log` | `d540a985ee3acf4e215a951b945fa0563894e01ea885de76f8cec93327f196b5` |
| `task-2-xfs-proof3.log` | `7b3825e4b837b672d46218b6a0e8a7570a5b3e86683b47d5115784a6f5cae702` |
| `task-2-btrfs-proof3.log` | `fb4eb971041faeee89b5bb87ada8077f813372cf839106ac469c6ad26e03a997` |
| `task-2-windows-backend-check.log` | `e923dfbe12df1eba8cdc705ec4a9c28b790b69972671b146054e3ef9305ddefb` |
| `native-proof3-harness/source-sha256.json` | `3de419c93f25377c8f2a7583182e83bb140f3bbdc21a57aabc2582a0e81b0e30` |
| `native-proof4-harness/source-sha256.json` | `0aaa61c465910c3f5970e0c07e6432df43a46baff01065ca3199e4e1bea1f533` |

## Native Copy timestamp prerequisite — review pending

The timestamp prerequisite is implemented in the owned paths within mixed checkpoint `057457312`, with scoped cleanup `8449209aa`. Its exact owned review diff excludes the checkpoint’s unrelated peer changes (SHA-256 `cceca644caadc6226fa21656d7cdf661984ad1ff5ee74538d50af2110a6b9204`). Independent spec and quality review is pending.

Wire schema 3 carries exact signed 100 ns access/data-change values or a typed unavailable reason. Native parser reports remain schema 2. Extraction accepts positively covered, uniquely identified unnamed STANDARD_INFORMATION with supported 48/72-byte layouts; malformed-report tests supplement the actual-image extraction tests. Public support gates remain unchanged.

Verified evidence: protocol 5, helper unit 5, native image 21 and native session 15 tests passed. Real v5 images cover both layouts, fractional timestamps and a time before 1970, with full-image hashes unchanged. The controller independently verified both 64 MiB fixture hashes against the committed receipt. Host and Linux strict helper/protocol lint, Linux cross-build, Windows no-native-activation check, standalone package extraction/build, 42 host endpoint tests and endpoint platform checks passed. Full host Imager test compilation passed again in 35.37 seconds, with the existing unrelated UI `next_wake` warning. Endpoint harness warnings remain separately recorded; these are not native platform runtime tests.

This supplies timestamp facts only. Shared Copy staging, two independent native source passes, native V3 durable receipts, explicit fresh-selection resume and the administrative production command still require implementation. Native Linux/Windows runtime acceptance, broader Connected behavior, installed Desktop lifecycle and Full OS staging/dual boot remain open. The held VMs were not operated.

### Timestamp review closed; Copy integration started

The independent review required actual malformed-image tests across the parser boundary. Scoped fix `96f34b461` adds raw missing, duplicate and invalid-flags Standard Information fixtures. The missing attribute causes structural parser refusal (status 5, incomplete metadata, no native identity); no readability claim is made for that image. Duplicate/invalid-flags fixtures pass parsing and retain readable content while producing typed timestamp unavailability. Whole-image hashes and zero native writes are checked. No production parser or verifier behavior changed in this fix.

The final v4 native-session suite passed all 16 tests in 53.46 seconds; strict helper lint passed. The controller independently verified all five final timestamp fixture hashes/sizes. Fresh scoped re-review marked the finding addressed with no new Critical/Important breakage. The timestamp prerequisite is therefore source-accepted; native Linux/Windows production acceptance is still unrun.

Substantive Copy integration is now in implementation: shared verified staging, two native source passes, native V3 durable receipts, explicit resume with a fresh authenticated source selection, and a Linux administrative command. These are ongoing deliverables, not completed behavior. Existing local journal compatibility, no-overwrite behavior and production support gates remain requirements.

### Native Copy implementation — recovery review requires fixes

Commit `2a96f7ff773a1d05798e4031a9e33f5a6ba40752` implements the native consumer, shared Copy staging, native V3 journal and explicit fresh-selection administrative commands. Its scoped review covers 17 owned paths; the raw review diff SHA-256 is `d063d5511a575fca7d97789810e4d77eab4df17a07f0121660d57debcd72ebaa`.

Real image diagnostics exercised native source reads through staging and durable receipts. The strengthened tree test passed with 30 native ranges and 12,587,213 verified ordinary bytes, including an empty directory. Unsupported NTFS system metadata remains pending. The final focused contract suite passed 9 tests with 2 separate image tests ignored; helper unit tests passed 5, and the standalone source-package build test passed. The controller independently checked all eight original and eight derived fidelity-image hashes against the committed fixture manifest. Scoped host/Linux lint and Windows compilation completed with disclosed harness warnings; whole-app strict lint is not green because of unrelated peer errors. These checks do not establish production runtime acceptance.

Independent spec and quality review found four recovery defects: destination edits during blocking postflight can escape validation and have their modification time overwritten; completed directory timestamp changes can be overwritten; a journal moved during source validation can be repaired before its location is rechecked; and receipt-history replay lacks a total work/size ceiling. Source approval is withheld. The original implementer is addressing all four in fix round 1 with focused regression tests. A smaller generated-fixture naming mismatch is also recorded.

All production Linux device fixture modes and Windows runtime acceptance remain unrun. Neither held Fedora VM was operated. Public support gates, Connected synchronization, installed Desktop lifecycle and Full OS staging/dual boot remain unfinished.

### Native Copy recovery review closed

Fix `1f6c4d993363866f6d1e66640af559124338ab7a` addresses all four recovery findings and the fixture lookup mismatch. Fresh scoped re-review marked every finding addressed and identified no new breakage. The native Copy consumer source subtask is accepted; this does not complete Task 2 or native runtime acceptance.

Resume revalidates file handles and names after destination hashing and blocking source postflight, restores access time only, and refuses changed completed-directory timestamps. Journal repair rechecks its pinned location immediately before truncation. Native history has checked aggregate frame/byte limits and cancellation checks; outcome groups reserve their full capacity before append. Exhausted recovery history preserves output and refuses continuation rather than silently renewing its budget.

The fix passed 95 focused regressions, 20 native-contract tests, two real-image diagnostics, and the generated-fixture lookup regression. Host/Linux scoped lint, Linux backend/command cross-linking and Windows compilation completed; inherited harness warnings remain disclosed. The full host Imager check also completed. Behavioral failure logs precede the fixes, including file edits, changed directory timestamps, journal relocation, unbounded history and cancelled replay/append. Production runtime tests remain unrun.

Source design for the planned durable Connected store is next. It must preserve endpoint capability gates, retain conflicts and recoverable versions, and fence disconnected connections across restart. Native write/durability proof remains required before public synchronization can be enabled.

### Connected store implementation in progress

The accepted source design now has an active implementation under `connection/store`. Five actual Imager tests passed in 74.49 seconds: private store initialization and exclusive locking, connection/absent-baseline persistence, refusal to finish disconnect with pending intent, disconnect fencing and stale acknowledgements across reopen, and bounded paging through 4,097 durable connection records. This is an implementation checkpoint, not independent source acceptance or native durability proof.

Two following tests expose the still-missing recovery reservation/byte-preservation behavior. Recovery files, cancellation, retention accounting, corruption and crash-boundary coverage, platform checks and independent review remain in progress. Production receipt-to-publication authority is intentionally absent until real native displacement/write-epoch evidence exists. Windows durability and all previously listed runtime/integration requirements remain open.

### Connected store source handoff — independent review pending

Commit `87871603f46612809c2188fdbce76824ffc1c5c5` completes the current private store source slice. Its six-path review spans baseline `d09e7733c` through that commit, including intermediate owned source captured in peer `4cd079801`. The scoped review package SHA-256 is `cbdca7b04f749ff82499c4b574425a432da969ad27e0ea2067b1e444b3927ef1`. Independent specification and quality review is in progress; source acceptance is withheld pending that review.

The implementation adds bounded pages/checkpoints and replay, durable intent and disconnect fences, actual recovery-byte preservation with independent reread, conservative retention, and incremental reclamation of unreachable private control objects. Reclamation captures objects without overwriting existing quarantine entries and validates the captured object before deletion. Production callers cannot manufacture publication or baseline evidence; native transaction integration remains separate.

The actual Imager store suite passed all 35 tests in 170.80 seconds. A concurrently running exact-source harness also passed 35 tests in 192.04 seconds. Both preceded a final orphan-log error-classification adjustment; the final actual Imager regression passed in 0.17 seconds, with byte/fence preservation checked. Model 9 and reconciliation 26 tests, actual Imager compilation and Linux/Windows source checks passed. Final scoped Clippy completed with disclosed warnings; it was not warning-free. No unchanged suite was rerun solely to regenerate evidence.

Checkpoint reclamation can perform up to 32 streaming reachability traversals and needs performance scrutiny for large stores. Host filesystem, fault-injection and process-interruption tests do not establish native power-loss durability. Native Linux/Windows execution, write epochs, displacement/publication proofs, directory recovery, worker/service/UI integration and the broader migration requirements remain open. Both Fedora VMs remain on hold, and public capability gates are unchanged.

### Connected store review — two fixes required

Independent review found two Important defects in the source handoff. First, journal admission estimates memory growth from the event size even when the reducer reconstructs a much larger stored connection row. The event can become durable before the actual overlay or control-key limit is checked, leaving replay unable to reopen the store. Second, checkpoint publication validates retained CURRENT and active-log state only before page construction; changes during preparation can be overwritten or omitted when the new checkpoint is selected.

Source acceptance remains withheld. Fix round 1 requires exact prospective row/key admission before persistence, reevaluation after an implicit checkpoint, and fresh retained-state checks at checkpoint publication. Focused regressions must cover long-locator fence pressure and control-state changes during checkpoint preparation, followed by independent scoped re-review. The review did not rerun existing suites or operate either held VM. Native Windows/Linux runtime and broader integration requirements remain open.

### Connected store fixes — scoped re-review pending

Commit `cbea72f31ce9c4e366ea92e30558c9333832e295` adds exact replacement-aware admission before journal writes and retained-state checks immediately around checkpoint publication. The actual Imager covering suite passed all 42 tests in 195.46 seconds. Final test-only amendments passed focused supplements (two checkpoint tests and one replacement-accounting test). Linux/Windows source checks and scoped Clippy completed; existing warnings and unrelated application lint failures remain disclosed.

The initial long-locator regression had an invalid fixture and is not counted as behavioral failure evidence. Its corrected admitted locator was tested against the original implementation and failed at the disconnect fence, then passed with the fix. Checkpoint tests distinguish identity replacement from content change, check both publication boundaries, and verify unexpected log bytes survive reopen and attempted reclamation. Native platform runtime and power-loss proof remain unrun.

A fresh scoped independent review is underway. Its exact fix package SHA-256 is `b0dfe57a13a4cec4827db98c42967ae1c9ff76c52a4f18b5f6dfc43fc3307f51`. Source acceptance remains withheld until both findings close. Native regular-file mutation design is prepared for the following source work; Windows durability, writable NTFS qualification, directory recovery and service/UI delivery remain explicit obligations.

### Connected store source review closed

Fresh scoped re-review of `cbea72f31` marked both Important findings addressed and found no new Critical/Important breakage. The private store source slice is accepted. Exact row admission now precedes persistence and is recomputed after checkpointing; checkpoint publication validates retained pointer/log evidence before replacement and before adopting the new generation.

This does not complete Task 3 or establish native Windows/Linux persistence. Actual native mutation evidence, platform runtime qualification and service/UI integration remain required. The next source slice implements Linux regular-file primitives and an isolated lease receiver, with real native test entry points and honest execution limits while both Fedora VMs remain on hold. Windows durability/executor and writable NTFS work remain part of the delivery scope.

### Native regular-file primitives — implementation checkpoint

The Linux native source slice is in implementation. Five shared protocol/admission tests passed through the actual Imager target after behavioral failures were recorded. These cover bounded messages/guards, stale responses, permanent revocation and sequence exhaustion. The exact-source Linux test executables and a separate normal lease-helper executable cross-linked successfully; the helper wrapper calls the same startup dispatcher wired into Imager. This is neither a shipped Linux package nor runtime acceptance.

Native tests are being added for no-replace publication, actual displaced objects, concurrent writers/mappings, lease notification, process failure and IPC bounds. They remain unrun while both Fedora VMs are on hold. The source slice produces real absent-name publication and quarantine evidence; replacement authority still requires later verified recovery/store integration. Windows persistence design is prepared separately, preserving full native file IDs, compatible sharing modes, existing Unix formats and the unresolved native durability/authority prerequisites. No public capability gate changed.

### Native regular-file primitives — source handoff under review

Commit `9ca3b22ed5a33911e19ea06515a5188436b104da` delivers the eight authorized source files. Independent specification and quality review is underway against dispatch base `f2f2aa0e3a439429ec808ce49e7f26f966930797`. The scoped package SHA-256 is `b69337d75dc3d8ebe4b166b0f931c3a3f2716674cc8ae452f3ea535d9500ce0e`; its raw diff matches the implementer's handoff. Source acceptance remains pending.

Six shared behavioral tests and all 90 actual Imager connection tests passed. Final Linux test and normal helper cross-linking completed in 4.65 and 2.51 seconds; scoped Clippy completed with disclosed warnings. Windows compilation confirms the Linux producer is inactive on that target. The final helper SHA-256 is `0a808adf00bf888effc08413ea61f266f9bb486580fcff2626e40bb31728d907`; the native test executable SHA-256 is `deb710598021187a1b460f62c056c61a724ca85982354d6cdcb64aa9b6051feb`. Both are linked aarch64 Linux ELFs from the disclosed exact-source harness, not a shipped Imager package.

All nine native test entry points and their subprocess cases remain unrun. They cover actual publication and quarantine races, old writers and shared mappings, isolated signal handling, lease interruption, malformed IPC, child/parent failure and forced lease breaks. Native filesystem qualification, crash/power-loss evidence, recovery/store integration and replacement authority remain open. Both Fedora VMs remain on hold. Full Windows/Linux migration, Connected service/UI, installed Desktop lifecycle and Full OS internal staging/dual boot are still incomplete.

### Native primitive review — two fixes required

The scoped review found two Important issues. Publication can return success for a nested destination parent moved during its final file reread, because the final name check uses the retained old directory without revalidating its ancestry. The parent-death regression polls with conflicting writer opens, which themselves trigger lease breaks and can mask missing socket-EOF cleanup.

Fix round 1 is assigned to the original implementer. It requires final target ancestry/security validation with an uncertain result and retained bytes on failure, a deterministic native ancestor-substitution regression, and independent observation of receiver EOF cleanup or termination before attempting any writer open. Source acceptance remains withheld; fresh scoped re-review follows the fix. No native tests were run or runtime gates changed.

### Native primitive source review closed

Fix `27dd198915e9f4deee743b85feea81847070a0f2` closes both Important findings. Fresh scoped re-review found no new breakage or out-of-scope observations. Final publication now revalidates destination ancestry/security after the long reread and source postflight; failure retains the uncertain result and bytes. The parent-death regression observes the helper through a retained pidfd before any writer open can trigger a lease break.

The amended Linux native test executable and normal helper linked successfully, and scoped formatting/lint completed with the existing warnings disclosed. The final native test ELF SHA-256 is `4bb3101947ac3fe15cfecbcb1d66fb501a991d3d838dffaab8c344778f333746`. The fix review package SHA-256 is `146094d402929584c7ba581d985d31903d8d95f7a0117f224fedad352b8180a8`. All ten native entry points remain unrun; this accepts the source slice without completing Task 4 or native qualification.

The next source slice is the Windows Connected persistence backend. It must preserve existing Unix store formats and behavior, use full Windows object identities and role-specific sharing, and implement actual relative opens, namespace mutation and authenticated volume barriers. Live authorized access, platform persistence qualification and a reviewed product authority provider remain prerequisites to Windows activation. Neither held Fedora VM was operated.

### Windows persistence — implementation checkpoint

The Windows backend is in implementation against source baseline `86b543af3`. Four shared tests now pass through the actual Imager target: full Windows object identity, exact legacy Unix encoding with strict identity-field handling, NT path components and checked trailing-buffer layout. Behavioral failures were recorded for the missing Windows identity shape, explicit-null mixed identity fields and empty native components. These tests exercise shared logic, not native Windows I/O.

The exact-source Windows backend/store harness compiles successfully. An actual Imager Windows check separately fails while compiling `ring` because the host C toolchain lacks Windows `assert.h`; the earlier host `runtime-portal` dependency failure was fixed by a peer commit and actual host Imager tests now run. A bounded read-only toolchain preflight is investigating a complete Windows cross-build. Neither harness compilation nor these host tests establish a shipped Windows package, native sharing/ACL behavior or power-loss durability.

Native fixture entry points, full store regression coverage, final handoff and independent review remain in progress. The next Linux transaction/store integration design is prepared for real regular-file updates, recovery, conflict handling and restart continuation after shared ownership is released. Both Fedora VMs remain on hold, public write gates remain unchanged, and the full Windows/Linux migration objective remains incomplete.

### Windows persistence — source handoff under review

Commit `bc2b393d1193773d0f15ca6204b0e6c28bd241d6` delivers the 13-path Windows persistence source slice. The independent specification and quality review is underway against baseline `86b543af3c4a375f82e6242475350918807ab843`. The review package SHA-256 is `8c55598606900b2690f10b3774e575e17242a798f2f3baddc44e65debae930f3`; its exact raw diff matches the implementer's handoff. Source acceptance remains pending.

The actual Imager connection suite passed 96 tests in 248.36 seconds. Four final GC regressions passed in 1.25 seconds after preserving Unix capture behavior and handle lifetime. Windows and Linux source/test checks completed in 2.72 and 3.26 seconds. Final scoped Windows Clippy completed in 3.28 seconds with a command-only allowance for the existing Copy `unreachable!` diagnostic; 206 library, 56 library-test and one helper warning remain disclosed. These are source checks through the documented exact-source platform harness, not a linked or shipped Windows application.

All 15 native Windows entrypoints remain unrun. Actual ACLs, sharing and writable mappings, volume barriers, rename/disposition, media incarnation and power-loss behavior still require qualification. Public Windows store admission refuses before creating state or opening devices; the live authority provider remains missing. Both Fedora VMs remain on hold. SDK build-tool installation and Microsoft's linked license acceptance were separately presented for confirmation and remain pending; no SDK download or use occurred. The Linux transaction/store integration and all previously listed migration requirements remain unfinished.

### Windows recovery verification — source review requires a fix

Independent review found one Important defect in `bc2b393d1`: standalone recovery verification returns a permissive Windows reread handle without retaining the original writer or another stability guard. A compatible editor can change or delete the object after verification while the returned reader remains live. Initial preservation already retains its writer; the reopen path needs equivalent protection. The original implementer owns fix round 1, including a native regression that holds the reopened reader across editor/deletion attempts. Native execution remains unrun and source acceptance is withheld pending fresh scoped review.

Two minor items remain recorded for final review: repeated volume-wide GC barriers within a single step, and the disclosed warning/lint baseline. Runtime sharing, media-incarnation, power-loss and packaging requirements remain open; they are not resolved by source review.

The next Linux integration descriptor preflight is complete. Explicit whole-file bounds are 15,688 bytes for QNI2, 444 for QNS2, 898 for QND1 and 5,435 for QNO1, all below 16 KiB without reducing existing path/component/recovery-slot limits. QNO1 binds exact durable recovery facts through bounded references; those references still require fresh store and native verification and confer no authority themselves. Implementation remains queued until the Windows fix review releases shared files.

### Windows recovery fix — scoped re-review pending

Fix `76ea77c4cfc4230d7bd80f50079c774c72532619` changes four owned files. Reopened recovery readers retain a stable Windows handle; independent rereads of live preservation retain the original writer through a private typed reader and its clones. A bounded, session-local weak registry matches actual recovery/object identity and never recreates authority from persisted rows. Existing reducer consumers retain verified readers across the dependent journal append, automatic checkpoint, flush and in-memory commit. Unix retains its existing reader and wire behavior.

Ten focused actual Imager regressions passed, including recovery, publication-authority refusal, baseline visibility and faults, fence retention, and the automatic-checkpoint candidate case (68.70 seconds). Final Linux and Windows source/lint checks completed in 4.47 and 4.15 seconds with the disclosed Copy command allowance and warnings. Formatting and scoped diff checks passed. No unchanged broad suite or native fixture was rerun. The Windows native regression now checks live/reopened/cloned reader protection against compatible editors, deletion and rename, with after-drop success controls; all 15 native entrypoints remain unrun.

Independent scoped re-review is underway. The exact fix package SHA-256 is `5d9d3853cb5ea89f929e3ded6bdb6dcc183598774cc5246cff61bb1b4dcb0243`; the raw diff matches the implementer's report. Source acceptance remains pending. Missing Windows authority/qualification, SDK confirmation, VM hold and the broader migration obligations remain unchanged.

### Windows persistence source review closed

Fresh scoped re-review of `76ea77c4c` marked R1 addressed and found no new breakage. Stable reopened readers, protected live-writer rereads and their clones retain the required handles; reducer checks keep them through the dependent journal durability boundary. The Windows persistence source slice (`bc2b393d1` plus `76ea77c4c`) is accepted. Repeated GC barriers and the disclosed warning baseline remain deferred minor items for final review.

This does not complete native Windows support or Task 4. All 15 native entrypoints remain unrun, and live authority, sharing/mapping, media/persistence qualification and full application packaging remain required. The next source slice joins the accepted Linux primitives to the durable store for real regular-file synchronization, conflicts/restore, interruption recovery and verified baseline progression. Both VMs remain on hold, and public support gates remain unchanged.

### Linux integration checkpoint and Windows authority preflight

Linux transaction/store integration is active. Its full scope remains successful regular-file synchronization, conflicts/restore, restart continuation and verified per-path baseline progression. An independent checkpoint review will first cover complete descriptors and the durable/live evidence join; the same implementer then continues the executor and baseline work. The first two actual Imager descriptor tests now pass after failing on valid-record decoding. The initial package baseline run also completed with 96 passes in 270.10 seconds. No complete integration checkpoint or native-runtime result is claimed.

The Windows authority preflight identified a documented limit: creation of a writable mapped section breaks modern oplocks without waiting for acknowledgment. Oplocks alone therefore cannot supply the desired exclusion. Modern Read oplock acquisition does explicitly reject existing writable sections and requires asynchronous I/O. The proposed combination of retained sharing restrictions, section-aware admission and sealed application handles remains an inference to qualify, not a completed guarantee. [Microsoft section-synchronization rules](https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/fs-filter-acquire-for-section-synchronization2), [oplock acquisition requirements](https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/granting-oplocks).

The bounded follow-on proposal includes continuous same-handle ownership, protected independent rereads and rename transitions, completion/cancellation resource lifetime, and native tests for surviving mappings and pending I/O. Private recovery artifacts need the same mapping qualification; the accepted guard-lifetime fix alone does not establish it. No Windows executor implementation or runtime action was dispatched by this preflight. Both VMs remain on hold, SDK confirmation is pending, and public capability gates remain closed.

### Integration recovery and focused admission tests

After an environment interruption, the original integration agent was no longer registered and no relevant build/test process remained. The existing source, exact-path diff and test evidence were preserved before a replacement resumed the same checkpoint. The full integration scope and independent review requirement remain unchanged.

The actual Imager now has passing focused evidence for all four descriptor layouts and exact recovery facts, store/session admission, immediate disconnect closure, durable namespace reservation, and preparation of a new file without invented recovery bytes. Two further recovery tests passed: a closed session cannot begin a new intent before the durable fence, and namespace context construction rejects stale attempts/sessions. A later refinement of that context test exposed historical session bytes incorrectly tracking the native epoch; that iteration remains in progress. These results establish shared host behavior, not native mutation or a completed integration checkpoint.

The missing temporary compile harness was restored from retained inputs. Its library and helper sources are byte-identical to their snapshots; the manifest includes the three Windows features already approved for the persistence backend and exactly matches Imager's Windows dependency declaration. The restored manifest SHA-256 is `635b6b882beea5ae5683b416047d3a73f58a64afb6232d69ba659ee34829fd13`. Offline metadata and wrapper syntax checks passed. No build artifacts or lockfile were recovered, and restoration itself provides no current compilation/runtime evidence.

The descriptor session field may use a store-owned process-id/counter tag for historical correlation. It is explicitly not globally unique and must never grant authority: the live Arc/Weak marker, fresh native attempt/epoch and exact revision/fence joins remain required. Cross-process tag reuse may make diagnostics ambiguous; if that proves material, the tag scheme needs revision. Actual guarded byte preservation, the remaining native producers, checkpoint review, executor/restart handling and baseline completion are still open. Both VMs remain on hold, and SDK confirmation remains pending.

### Native/store integration checkpoint 1 — independent review underway

Commit `48e6add90091a58b657626bcfaac4b0014edfc18` delivers the ten-path Section 7.1 source checkpoint: bounded descriptors and namespace registration, live store/step admission, actual guarded native-byte preservation with independent private reread and durable acknowledgement, displacement sealing, and exact Connected observation exclusions. Unknown or replaced qOS-like names remain visible and make the Connected observation incomplete. The overall integration remains incomplete; successful whole-operation execution, conflicts/restore, fresh restart reconciliation and baseline completion are Sections 7.2–7.3 after this review.

Root verified the exact commit scope, clean owned worktree, all ten committed source hashes, both Linux artifact hashes and all 13 native entrypoint names in the binary. The raw diff SHA-256 is `674d5bae11998640def56a7489a9306b060103a66057c5aca72cc33148941b26`; the scoped review package SHA-256 is `d8f86365a56d1b3e044e1bfbd3de8ec7b86fa98784175610e0f3c5429b16eedb`. Independent specification and quality review is underway against original base `a25bd9c0f4198363c5d8f8475cc525deebe89778`. Source acceptance is pending.

The actual Imager connection suite passed 111 tests in 284.48 seconds. After the final shared-budget and lint refinements, the transaction group passed 14 tests in 0.01 seconds and the native-store group passed eight in 1.47 seconds. Final Linux test and normal helper builds completed in 5.84 and 3.36 seconds. Linux and Windows source lint checks passed with documented warnings; Windows retains the previously disclosed command-only allowance for the existing Copy diagnostic. Scoped formatting and the committed diff check passed. No unchanged broad suite was repeated after the focused refinements.

The final native test ELF SHA-256 is `6421dcb8841130b24d27d02721ea82b35b70bbe0b139f2537855ac3403beca8a`; the helper SHA-256 is `6abad26295c4382e5b6141ddb66526a15d916be88681bc5270194edb4a40c88c`. These are exact-source harness artifacts, not a shipped application. All 13 native entrypoints remain unrun, including the three new preservation/observer, late-disconnect and source-loss cases. Native qualification, Windows packaging/runtime, installed Desktop lifecycle and Full OS internal staging/dual boot remain open. Both VMs remain on hold, SDK confirmation is pending, and public support gates remain closed.

### Checkpoint 1 review — admission and observation fixes required

Independent review found two defects in the source checkpoint. The store retains only a weak disconnect gate, so dropping the last external session can discard the closed latch and permit a new session before the durable fence. Excluded-namespace validation checks directory identity but lacks a content snapshot around its reads and enumeration, so an unknown child or replaced descriptor can be hidden during the final observation check. Root checked both reported code paths and assigned the original implementer fix round 1. Source acceptance and dependent executor work remain pending scoped re-review.

The fix requires host regression coverage for closing, dropping and reacquiring the session, plus a deterministic native fixture for a concurrent excluded-directory change. The native fixture will be compiled only while the VM hold remains in force. Local Clippy cleanup items are recorded as deferred minor findings for final review.

The review also clarified a later integration obligation: existing outcome/recovery verification rereads need the operation's budget, deadline and cancellation checks while retaining their platform guards through durable work. A separate read-only preflight is resolving accounting across process/store reopen; no reset of the canonical operation allowance is assumed. Restore and conflict-resolution selections will remain private transaction/store data rather than extending the pure reconciliation decision model. These are preparation for the unfinished Sections 7.2–7.3, not completed runtime support.

### Checkpoint 1 fix round 1 — scoped review underway

Commit `0f9d623756072b6705652e391f8b5ef24336f505` changes five owned files. Store-owned admission gates retain the stop latch across external session drops. Per-observation excluded-directory snapshots retain native identity and metadata through descriptor reads, enumeration, exclusion and the final scan, including a final recheck of earlier namespaces. Fresh independent review of both fixes is underway; source acceptance remains pending.

The two disconnect regression tests first failed on the reported assertions, then passed in 0.16 seconds. Final actual Imager checks passed ten store tests in 1.77 seconds, fourteen transaction tests and seven endpoint tests. Linux test and helper links completed in 8.79 and 3.37 seconds; Windows source and lint checks completed in 2.19 and 3.45 seconds. Scoped formatting and diff checks passed. Disclosed warnings and the existing command-only Copy lint allowance remain. No unchanged long suite was repeated.

Root verified all ten committed/worktree source hashes, five-path scope, clean owned source, exact fix-diff equality, both final artifact hashes and all fourteen native entrypoint strings. Fix diff SHA-256 is `29ba2bcf6519fc45d3f6619c31ee33d1265b394b28239d5329a1e594c7eb8488`; scoped review package SHA-256 is `d8b44c5ccf94e2047579274eab9c3e89365b5eba256d6555cfe2d666dc9eea4e`. Test ELF SHA-256 is `cba4fcceae2483ed875568d64b20bd6e23e5bedb56b262994069598b7a507750`; helper SHA-256 is `7213d8986606f3675fa93f176989a6f303effd5f72847c0c529560204caf797c`. All fourteen native fixtures remain unrun, including the new deterministic final-observation race case.

The completed read-only preflight selected durable precharged read windows for later native restart work. Root accepted an explicit boundary: only durably marked native-budget operations may use structural historical replay; subsequent live verification must pay from the same canonical operation's remaining allowance. Legacy checks and Windows reader lifetimes remain unchanged. Reservations use serialized store ownership without holding the admission mutex through I/O or reopening a stopped latch. Unused credit is conservatively lost at reopen, so repeated interruptions can leave an operation pending rather than resetting its limit. This design is not implemented by the fix. Sections 7.2–7.3, platform qualification and the broader migration work remain open.

### Checkpoint 1 source review closed

Fresh scoped re-review of `0f9d623756072b6705652e391f8b5ef24336f505` marked both findings addressed and found no new fix-induced blocker. Root read the complete review and accepted Section 7.1 source (`48e6add90091a58b657626bcfaac4b0014edfc18` plus its fix). The store retains immediate disconnect closure, and exact namespace exclusions now retain directory-content snapshots through the observation. R3 remains a deferred minor for final review.

The same owner continues Sections 7.2–7.3 under the accepted restart-accounting addendum: actual whole-file operations, conflicts/restore, fresh restart and safe drain, and verified per-path baseline progression. Native fixtures remain unrun; source review does not establish platform/runtime or power-loss qualification. Both VM holds, the pending SDK decision, closed public support gates and all broader Windows/Linux migration obligations remain unchanged.

### Remaining integration — durable read accounting in progress

The remaining source slice now has focused host evidence for zero initial read credit, an atomic native-operation budget marker, shared reservations, and cumulative accounting through actual checkpoint/reopen. Unused credit is lost on reopen; later reservations continue the same canonical sequence and total. The initial valid-reservation test failed before implementation and then passed. Supplemental tests cover concurrent credit spending, stale/busy/cancelled reservations and the exact limit. These are working implementation milestones, not a completed source handoff or native continuation proof.

Marked historical replay now loads durable relationships while fresh verification refuses a changed recovery artifact and preserves its bytes. Its first actual Imager attempt was blocked before reaching the test by an unrelated UI conflict. The existing exact-source host harness then produced the expected behavioral failure in 0.23 seconds; after the fix and merge resolution, the actual Imager test passed in 0.30 seconds. Harness and application results remain distinguished.

The actual Imager native-store context group passed fourteen tests in 1.76 seconds. Two additional private-file tests passed in 0.23 and 0.17 seconds: copy, independent reread and fresh verification charge short and EOF requests, and failed reads consume credit while retaining their partial recovery object. These tests also reject an unbudgeted legacy-verification bypass, exhausted credit and cancellation. Root read the terminal outputs and focused test bodies. Native lease, mutation, complete restart, filesystem durability and power-loss behavior are not established by these host results.

An unrelated shared integration checkpoint, `ce302be5f70a3b1a0e0073159e046ab1271b5497`, captured six in-progress migration paths before merging UI changes. This task did not initiate that checkpoint or merge. The remaining review base stays `208fda62ab6310685d9e19cd7541e5d0ef3e9ac9`, so its complete scoped diff will include those captured edits and the owner's subsequent changes. No peer history is rewritten or treated as source approval.

Recovery-only admission, incomplete/safe-boundary accounting, full native operation/restart drivers and verified baseline progression remain in development. A read-only inventory maps the fourteen previously accepted native fixtures and their later execution prerequisites; it explicitly excludes the still-required Sections 7.2–7.3 matrix. Both VMs remain on hold, SDK confirmation remains pending, and no new native execution or product support is claimed.

### Remaining integration — recovery facts and per-path baseline regressions

Focused actual Imager tests now cover retaining incomplete recovery bytes through checkpoint/reopen, and reserving inspection credit for stopped work without reopening propagation. Their final passing runs completed one test each in 0.47 and 0.36 seconds. Earlier compile and fixture failures remain disclosed in the evidence logs. These tests exercise private files and durable accounting; they do not establish live native restart authority.

A historical outcome regression first failed on the missing transition, then passed in 0.37 seconds. It binds the outcome to its prepared stage and attempt, rejects a substituted nonce, and preserves Published/Verified facts across reopen while leaving the baseline absent. Native addition, replacement and deletion producers are being implemented separately. Corrected Linux source checks completed in 1.60 and 1.95 seconds; these checks neither link nor execute the new native fixtures.

The deletion admission regression first accepted an operation with no committed baseline, then correctly refused it after the fix; the final host test passed in 0.13 seconds. A baseline patch regression initially accepted a candidate that dropped an unprocessed old path. The corrected pair of host tests passed in 1.40 seconds: invalid omission is rejected, and two other paths advance separately through checkpoint/reopen while preserving the original entry and clearing each completed pending operation. Root read the actual terminal output and complete test bodies. These tests use historical native facts and real store pages; typed live comparison/candidate authority and complete native operation coverage remain unfinished.

Root also accepted one narrow ownership extension: optional metered native baseline/candidate page reads in `store/storage.rs`. The existing page reader hides requested reads inside `read_to_end`, so serialized byte-size estimates alone cannot account for short, failed or EOF requests. The new seam must charge before each explicit bounded read, retain existing identity/hash/durability checks, cover candidate collision rereads and both possible baseline reductions, and enforce finite page/traversal caps. Ordinary legacy/replay and global store-control metadata behavior remain unchanged. Conservative estimates may leave large or interrupted operations pending; they must not reset the canonical allowance or bypass verification. This records the implementation decision, not completed page-metering evidence.

The original owner remains the sole source implementer. Remaining review still starts at `208fda62ab6310685d9e19cd7541e5d0ef3e9ac9`, now covering eleven authorized paths including the narrow storage seam. No final remaining-slice handoff or independent source acceptance has occurred. Native fixtures remain unrun, both VM holds and the SDK decision remain unchanged, and Windows/Linux product delivery, full recovery integration and Full OS internal dual boot remain incomplete.

### Remaining integration — metered baseline page verification

The optional storage seam now has four new passing host regressions. A real private page read charges its payload request and EOF request; cancellation refuses the read. Injected short/interrupted reads around a real file, a real write-only read error and a finite local request cap all consume credit conservatively while preserving the source. A bounded iterator rejects a mismatched root count, charges its constructor and traversal reads separately, and charges candidate page reuse for the independent AlreadyExists comparison. Empty and oversized declared baselines are also covered. The first valid-page call failed before implementation, then passed in 0.20 seconds; the short/cap and iterator/collision pair passed in 0.19 seconds. The context group including the existing per-path baseline tests passed three tests in 1.41 seconds.

A separate actual checkpoint/GC regression passed in 0.63 seconds. Overlay pressure forces both baseline reductions: candidate and old-baseline reads debit exactly 65,536 requested bytes, the candidate remains pinned through checkpoint/GC, and the committed two-entry baseline survives reopen. Root read the complete test bodies and terminal results. Historical native facts support these store tests; they do not establish live native comparison or restart authority. The Linux comparison source check completed in 2.18 seconds, with native fixtures still unrun.

Root resolved initial-review ordering without weakening operation accounting. Pre-Intent planning may use the existing bounded read-only observer under its own limits because no canonical operation budget exists yet. Only explicit acceptance of a complete preview may produce an initial-review token bound to connection/session/revision/fence, endpoints, baseline and exact observation/selection. Admission consumes the token; stale or changed proposals require pending work or refreshed review. Every authority-producing check after Intent, including resumed work, remains fresh and paid from the canonical allowance. The cost of this separation is additional bounded read work for repeated previews; the private adapter remains subject to source review before activation.

The same owner continues typed preview/candidate integration and complete native recovery, conflict and disconnect paths. A separate read-only inventory is locating future worker/UI/session-package entry points; it authorizes no implementation or runtime action. The full Windows/Linux migration goal remains active and incomplete.

### Remaining integration — explicit review and application entry points

The initial-review admission regression now refuses an unreviewed first native intent. Three host tests passed in 0.33 seconds, covering explicit token consumption, stale duplicate rejection, persisted preview/selection facts through checkpoint/reopen, changed selection and disconnect/reopen rejection. A further regression caught a caller-supplied `initial_review: true` being accepted without the durable review fact. After the fix, the four-test context group passed in 0.53 seconds and the unreviewed baseline remained absent. Root read the full test bodies and results. These tests use a test preview producer and historical native facts, so they do not establish native observation or UI approval qualification. Typed-candidate Linux source checks completed in 1.99 and 1.80 seconds; native execution remains unrun.

The completed read-only UI inventory maps the existing Copy/Connected/Later model, setup gates, host Desktop importer, Files commands and Linux/Windows package entry points. Connected remains disabled in live setup, and the importer currently offers Copy/Later. Files lacks a connection panel/client; the Linux session hooks do not yet package a shared connection worker. The inventory preserves the approved Windows Full OS scope and does not introduce a Windows desktop-environment or Files port.

A separate read-only service preflight identified reusable launcher service descriptors and bounded Files request framing. Its recommendation is a headless Imager worker host with shared protocol/client modules, keeping Files independent of Imager internals. This remains a design recommendation: authenticated folder grants, consumer/session handoff, canonical store ownership, reconnect behavior and Windows IPC/lifecycle still need implementation and review. Existing collaboration or filesystem service transport does not itself authorize synchronization.

Root confirmed an additional required Windows launch fix directly in current source. Imager uses the launch specification's default runtime-attachment policy; the live UI runner calls `ensure_attached` before starting the GUI. Its non-Unix implementation rejects launch unless `QOS_RUNTIME_STANDALONE=1`, while the packaged Windows PowerShell launcher simply invokes the executable and forwards arguments. Thus the ordinary default Windows launch path currently reaches an unsupported-runtime error. This is source evidence, not a Windows execution result. A bounded host-launch policy fix and appropriate tests are required before Windows delivery, preserving trusted first-login and real service/grant checks. No startup policy or global runtime behavior was changed during this preflight.

The original implementer continues the remaining native integration under its eleven-path scope. No second source implementer, final source handoff or remaining-slice review has been started. Both VM holds, the pending SDK decision and all native qualification requirements remain unchanged.

### Windows host startup policy and native continuation

The bounded Windows startup fix is committed as `62ecc9e87fe48932035e2239c958575c38c4cd24`: Imager now disables automatic runtime attachment only on Windows through its existing launch policy. The exact one-line diff preserves non-Windows attachment, helper dispatch, trusted import checks and capability gates. Independent scoped review returned specification compliant and quality approved, with no critical or important findings. Three existing injected launcher tests each passed; the host Imager check completed successfully. Existing UI/Imager warnings and the pre-existing import-order formatting failure remain disclosed. These checks establish the shared policy contract and source placement, not an executed Windows GUI or package qualification.

The original native implementer explicitly saved and yielded its partial source before this separate fix began. It resumed after the Windows review closed, retaining the same eleven-path scope and remaining review base `208fda62ab6310685d9e19cd7541e5d0ef3e9ac9`. No native source acceptance, concurrent second source implementer, VM action or SDK use is implied by that sequencing.

A paid deletion-preflight host regression first failed because admission read the baseline before reserving canonical credit. The corrected pair passed in 0.31 seconds: metadata admission succeeds, then paid verification rejects a mismatched survivor. Root read the full test. It does not prove exclusion of actual native steps or relocation; those assertions remain required in the native matrix. An amended per-path baseline context test also passed in 1.55 seconds.

The source-only native addition/deletion fixtures now follow explicit initial review through publication, typed comparison, candidate commit and reopen. Deletion retains exact bytes in endpoint quarantine and private recovery before committing an empty baseline; later source edits invalidate a fresh final comparison. Linux source checks for these intermediate paths completed in 3.01 and 2.37 seconds. Subsequent restart reconstruction edits were deliberately incomplete at the sequencing boundary and remain in development. None of these fixtures has been executed, and no final remaining-slice artifact or acceptance is claimed.

Windows native build/package/startup, full Connected worker/UI integration, installed Desktop lifecycle and Full OS internal staging/dual boot remain unfinished. Both Fedora VM holds and the pending SDK decision remain unchanged.

### Restart reconstruction and remaining host integration boundaries

A new actual Imager host regression caught `PreparedNoDisplacement` being classified as recovery-only by a numeric stage comparison. It failed at the intended pending-work assertion, then passed in 0.34 seconds after explicit completed-stage classification. The test uses historical prepared facts and real checkpoint/reopen: old read credit is unavailable, a fresh reservation works, pending state and absent baseline remain unchanged, and disconnect still produces a recovery-only context. These facts do not constitute native resume authority.

A second host regression first failed with `Corrupt`, then passed in 0.41 seconds. An ordinary outcome event rejects historical progress, an explicit reconstruction event rejects future progress and accepts properly bounded historical progress, and Published state survives real reopen while the baseline stays absent. Root read the complete test bodies and terminal results. Test-only resume and fabricated historical descriptors are distinguished from the fresh native producer.

The addition restart fixture now covers six successful boundaries in both directions: staged only, publication without an outcome descriptor, outcome without Published, Published, Verified, and candidate preparation without commit. Eighteen further cases exercise changed source, competitor, equal bytes under a different object identity, unknown namespace child, substituted descriptor and a closed stop latch. Root read the complete fixture, including paid recovery context, fresh epoch, typed resume, comparison/baseline completion and retained-byte assertions. The latest Linux source check completed in 1.77 seconds; all native bodies remain unrun. Displacement/recovery-artifact rebinding and the full remaining matrix are still being implemented.

Read-only interface preparation confirmed another required folder-selection fix: Imager sends the portal's directory option, but the native qOS shell adapter currently omits that option from its picker model. Picker paths, existing in-process Unix grants and Desktop readiness replies also do not supply a shared worker with transferable Connected authority. Future integration requires explicit durable consent and fresh endpoint checks; a temporary unavailable source pauses and rechecks without becoming a deletion or forcing renewed approval solely for going offline.

The Windows IPC preflight proposes an unprivileged session worker, authenticated local communication, canonical store ownership and remembered consent with fresh validation. Its source inventory and Microsoft documentation are preparation, not accepted implementation or runtime proof. Windows host authentication remains separate from the existing Linux qOS first-login contract, with no Windows DE/onboarding port added. Both VM holds and the pending SDK decision remain unchanged.

### Replacement restart and deletion admission in progress

The native replacement-restart fixture now covers eleven boundary variants in both directions, from a still-live original through raw/durable quarantine, private recovery, interrupted acknowledgement, partial copy, publication, Verified state and an uncommitted baseline candidate. These variants are not a claim that every required design category is complete. Root read the complete fixture: the simulated prefix remains in its original retained-incomplete slot, a fresh slot contains the full old bytes, native quarantine retains the original, and both live files contain the intended new bytes after completion. The independent source check finished in 2.12 seconds with fourteen test warnings, three duplicated. Earlier borrow, fixture-visibility and mutability compiler errors remain disclosed. All native execution remains unrun.

A strengthened existing host deletion-preflight test caught an unchecked native step being admitted. It failed at that assertion in 0.22 seconds, then passed in 0.31 seconds after the admission gate was corrected. Root read the complete test and output. All five step kinds are refused both before paid verification and after verification rejects a survivor that differs from the committed baseline. This establishes host admission behavior; real-tree no-relocation and retained-byte assertions remain required in the native matrix. It is an extension of the earlier preflight test, not an additional unique passing test.

The original native owner continues deletion restart, conflict/restore, safe disconnect accounting and the complete remaining matrix. No final source handoff or acceptance has occurred. Full OS bundle export, host validation and internal-boot harness files remain absent, and the live setup gate is still disabled. Windows native mutation authority, complete worker/UI integration and actual platform qualification remain separate unfinished requirements; neither the VM holds nor the pending SDK decision changed.
