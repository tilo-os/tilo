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
