# SDD ledger — plan: docs/superpowers/plans/2026-09-17-runtime-auto-attach.md

Ruling: Implement the already-described all-app automatic-start/join design without another permission gate — user explicitly reiterated implementation and rejected discussion-only responses — if interpreted wrongly, local changes remain reviewable and reversible.
Ruling: Keep actual Cargo package names and implement at shared live startup, including custom and definition paths — user explicitly said all packages — wrong coverage would strand an app; require shared path tests and independent native launches.
Ruling: Reexec through the native host and reuse inherited descriptors rather than mutate UI environment or create another broker crate — avoids unsafe environment races and shell/ui dependency cycle — costs an extra exec at first unmanaged startup.
Ruling: Auto-build the existing host only when absent in a developer checkout; installed layouts find their shared executable — plain cargo run must not require a hidden manual build step — first launch may incur a host build and failures must be visible/bounded.

Existing isolated worktree verified: codex/macos-shell, git dir differs from common dir, not submodule. Concurrent History owns launch.rs setter open_app_global_state_with_args and its new test; preserve both. History acknowledges our run_with_args/dispatch_definition native startup ownership. No GUI hold yet.

## Preflight
| Pair/task | Producer and consumer | Finding |
|---|---|---|
|1/2|shared bootstrap/host CLI -> native proof|CLI modes/profile override explicit; proof must use independent launches, no manifest/fd injection|
|1/History|native startup before construction -> custom state opener/History admission|disjoint functions; preserve existing setter/tests and captured dirty baseline|
|1/old MCP|daemon server -> existing adapter|same McpServer, original manifest mode remains; client EOF must not kill shared daemon/apps|
|1 itself|live-only bootstrap vs diagnostic modes|must handle definition path before construction plus legacy/custom after command/diagnostic branches|
|2 itself|ordinary cargo run vs immutable provenance|record actual binaries; profile override isolates user data, binary override only explicit evidence selection|

Task1 pending independent transport audit conclusion; Task2 depends on reviewed implementation. Prior shared MCP feature remains complete; this is a new plan.

Task1 dispatched /root/autoattach_implementation gpt-6-astra BASE5e19d7011899d6db0553d5148bff9e9898fe3e8b. Independent read-only transport audit never initialized and was cancelled; no audit evidence claimed. Root examined SDK descriptor/identity load and launch paths; worker owns implementation.

Task1 progress: portal actual subprocess RED/GREEN5 verified; descriptor alias RED reproduced and fix pending final regression. Host --test integration accidentally builds unrelated Linux-only shell bins on macOS; worker correctly moved real-process tests under shell --lib with explicit prebuilt runtime binary, no unrelated source fixes.

Task1 UI GREEN30 inspected including default/custom/definition preconstruction attach, diagnostics and existing Photos History test. Controller found/fixed-via-worker aliased descriptor equality and --apps original-argv misrouting; scoped regressions required. Initial daemon core4/5 passed; remaining assertion is Darwin /tmp canonicalization, not runtime cwd change. Real SDK publisher and missing-host bootstrap tests underway.

Ruling: Preserve intentional private/local app modes via a generic native attachment predicate defaulting to shared, plus Browser-owned live-local construction — unconditional attachment would persist private browser page titles in broker effect receipts — costs a small explicit policy seam; normal apps remain automatic, and private regression evidence is required before delivery. Browser owner implements App::from_scoped_parts_local and Browser predicate/tests; our worker owns LaunchSpec::runtime_attachment predicate only.
Task1 host suite GREEN33/33 reported; controller inspection pending. Narrow collaboration FD CLOEXEC adoption included after Browser demonstrated child inheritance leak; Browser owns its child environment cleanup.

Task1 implemented c2d6045c8, owned11file commit; portal10/UI32/host33 and binary build outputs inspected. Original Task1BASE5e19d7011; package uses c2d6045c8^..c2d6045c8 because intervening external commits own no Task1 implementation. Review pending; native Task2 not started.
Ruling: Correct the plan's ambiguous no-download restriction to allow ordinary Cargo dependency fetching/settings during automatic source builds — forcing --offline can make a fresh app-only checkout fail because shell dependencies were never cached, contrary to automatic startup — cost is normal Cargo network/build work; no opaque runtime binary download or extra installer is added. Task1 hardcoded --offline is a plan-mandated defect for the fix loop.

Task1 review Needs fixes, task-1-review.md: Important forcedoffline + stale parent descriptor environment for ordinary app-launched children. Fix round1 original implementer resumed BASEc2d6045c8; no task2/native launch yet.
Ruling: Add shared automatic-transfer process provenance and honor it in startup and SDK descriptor discovery — an ordinary spawned child must not treat its parent's now-CLOEXEC fd numbers as its own authority — costs a small ownership marker and descendant regressions; true current/legacy inherited transfers must remain authoritative and malformed ones fail closed. Shared fresh descriptor preparation must clear ancestor provenance, while automatic exec stamps its own PID and selected profile.

Browser private boundary evidence inspected: target/browser-complete-ux/task10-cycle82-local-final.log exits with1passed/0failed (ambient-service tripwires/local effects/wake scope/AX extension); Browser independent review/artifact still pending. This is not actualGUI presentation evidence. Task1 fix portal RED captured expected failures.

Browser privacy prerequisite inspected and committed a3720f6ff: durable audit docs/superpowers/audits/2026-09-17-browser-private-runtime.md records independent source review clean, local fixture1/shared AX45/Browser234 passed, Clippy exit0. Native Browser private acceptance remains outside this runtime proof and is not claimed. Fix portal GREEN13 inspected.

Task1 fix round1 committed6dce082e8; portal13/UI32/host36 and host build green inspected. Scoped re-review /root/autoattach_fix_review gpt-5.6-sol dispatched against exact fix commit package; pending verdict. No GUI yet.

Task 1: fix round 1/5 (2 addressed, 0 open; commits c2d6045c8..6dce082e8, exact fix package excludes external commits).
Task 1: complete (commits c2d6045c8 and6dce082e8, review clean). Browser privacy prerequisite resolved separately; native GUI/action proof assigned Task2, visible-frame qualification not required by spec.
Task2 BASEac8ee5209d009ab38e539fcc4661f18e050e5e48; dispatch follows.

Task2 native proof source quiet window accepted by Browser and History/Photos owners; worker authorized snapshot, hostbuild and isolated ordinary four-app launches. No Browser/Keychain use. Release explicitly immediately after exact owned-process cleanup.

Task2 native3 proof inspected: all4 ordinary cargo launches distinct peers1..4; first Writer starts daemon28409; Calculator add-function succeeded; after Writer exit others/runtime/MCP survive; source unchanged, MCP0, sixknownPIDs absent. Native1/2 harness cleanup failures retained and unqualified; native3 succeeds after exactPID/reap correction. Source holds RELEASED to Browser and History/Photos owners. Task2 docs/report pending.

Task2 implemented e01fc4809; root independently ran6focusedPython tests exit0 and verified native3proof, exactPIDabsence/profilegone. Task2 review /root/autoattach_proof_review gpt-5.6-sol dispatched exact owned commit package.
Ruling: Final review covers every change owned by this automatic-runtime feature plus its relevant Browser privacy/packaging integration, excluding unrelated concurrent work on this long-lived branch — a whole branch diff would mix other users tasks and obscure this acceptance decision — cost is that unrelated branch changes retain their separate review responsibility.

Task2 review SpecPASS/QualityNeedsFixes: reusable proof missing supported snapshot preparation sequence; profile deletion errors suppressed. Current native3 acceptance remains valid (profile independently absent). Fixround1 originalworker resumed BASEe01fc4809; no runtime source change or newGUIproof required for these harness/doc fixes.

Task2 fix1 committed9c6de88b3; report8tests+CLIcapture+realprofileremovalpass inspected; scoped reviewer /root/autoattach_proof_fix_review dispatched exactfixpackage. Runtime/native3unchanged.

Task 2: fix round1/5 (2addressed,0open; e01fc4809..9c6de88b3).
Task 2: complete (commits e01fc4809 and9c6de88b3, review clean). Root fresh8testspass; native3 unchanged.

Final wholefeature review /root/autoattach_final_review gpt-6-astra: oneImportant configured-Cargo-target hostartifact discovery, allotherrequirements pass. Single finalfixwave /root/autoattach_final_fix gpt-5.6-sol dispatched fullfinding; scope runtimeportal bootstrap/tests/dependencyifneeded. Existing native3 valid for testeddefault configuration, noGUIreplay mandated.

Final fix2acfd7d8c: portal14+productionClippypass. Scoped finalreview clean; allfeaturefindingsclosed. Worker workspacefmt incident remains recovery task before delivery; root restored218files exactpriorQA85hash+format-equivalence and approvedHistorylaunchsnapshot, Browser restoringownedcycle86source. No unrelatedchangesstaged.
Ruling: Recover accidental workspace formatting only from verified pre-format bytes or owner-reviewed format-equivalent reconstruction, with saved current backups and guarded writes — preserve every concurrent semantic change — cost is recovery time and explicit limits for any remaining formatting differences.

Recovery complete:218exactQA85 paths+2token reconstructed exacthash+23exactGitblobcopies+approvedHistorylaunch restored; Browserown3files recovered, Photos/ImageViewer unchanged, Imageractive13paths excluded byownerrequest. All sourceholdsreleased. Allguardedwrites preserved semanticcontent. Finalfeature reviewandscopedfix clean, noopenfindings. Root8prooftests freshpass; portal14 finalcheckqueued behind existingCargolock.

Final verification after guarded recovery: root cargo test -p runtime-portal --test native exit0 (14passed), proofPython unittest exit0 (8passed). Allrequired tasks/reviews closed. No push/merge; current named worktree/branch retained.
