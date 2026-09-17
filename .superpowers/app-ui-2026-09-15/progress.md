# All-app UI balance task recovery map

User explicitly requested all-app visual/source review and marking, THEN subagent fixes for framework and apps. This is a new task after previous framework goal completed. Do not create a new goal automatically.

## State

- BaselineHEAD47fc2425a643a07ebf9c4f127fb5579ba42f77a7. Existing isolated linked worktree codex/macos-shell.
- Review gate COMPLETE:48apps,47freshvisual+1windowlessAgent; allthree reviewers finished including LinuxWindows/OSK and supplementalroutes. No fixworker dispatched before gate.
- Baseline UXbench48pass0fail0error; desktopjourneys alone missvisualdefects.
- Reports and closure register: docs/superpowers/audits/2026-09-15-all-app-ui-review/.
- Binding spec: docs/superpowers/specs/2026-09-15-app-ui-balance.md.
- Capture-fidelity plan RUNNING: docs/superpowers/plans/2026-09-15-app-capture-fidelity.md; independent ledger .superpowers/sdd/2026-09-15-app-capture-fidelity/progress.md; worker /root/capture_fidelity. Needs scoped independent review when done.
- Read-only architectureworker /root/ui_balance_design writes framework-design.md here.
- Read-only appplanworker /root/app_balance_plan writes app-fix-design.md here.

## Next work — do not stop at planning

1. Review capture-fidelity implementation and fix findings. Rebuild/review truthful2x for previouslyinvalidhandlers.
2. Turn framework-design.md into bounded frameworkimplementationplan and execute sequentialfreshimplementers + scopedreviews.
3. Turn app-fix-design.md into boundedappplans; fix allmarkedP1/P2 ordinarysizeandlargeText, preserveeveryrouteandmanualcontrol. Source-only hypotheses are not forcedredesigns.
4. Freshallappcaptures andaffectedinteractionchecks, updateclosureforeachfinding, relevanttests andUXbench. Broadfinalreview.
5. Restore Fedora44ARM64 VM to suspended (was suspended before this task resumed it). No push/merge.

## Preservation / rulings

Unrelateddirtybaseline: .cargo/config.toml, Cargo.toml, Cargo.lock, DEVELOPMENT.md, apps/settings/tests/category_nav.rs and control_interaction.rs, crates/runtime/runtime-fs/src/operations.rs and recovery.rs, ui/src/text_render/renderer_regressions.rs, ui/src/widget/column.rs(formatonly), ui/tests/design_token_audit.rs. OtheruntrackedGTM/manifesto/docs/regression/scratchpad/plans/paths/cacheuntouched.
Scratchpad owner task Build macOS component demo app is finished/notLoaded. Newuserrequestauthorizesallapps but preserveoriginaluntrackedfeature; originalsrc/lib.rs backedupin scratchpad-original/. Prefer frameworkfix and minimalapplayoutpatch, do not blanketstage originalapp or manifests.
Existingmanualcontracts remain; do not changeglobalpaddingor legacyrenderer fontsemantics. No automatedpixel-pass is visualapproval.
Review scripts from skill lackexecutablebits; bash sdd-workspaceworks; task-briefcallsnonexecutablechild so directextraction used andrecorded inplanledger. Do not changeinstalledskills.

## Evidence paths

inventory.json; captures/<app>/{wide,compact,narrow,large-text}.png; supplementslabel-wide/narrow; manifestsbinaryhashes+source; capture-dimensions.json228PNGs; linux-source-hashes.json3614files; bench-baseline/summary.json. Linuxsource/home/parallels/qos-ui-review-20260915 isolated, cache/home/parallels/qos-target reused. Guestoriginalcheckoutsuntouched.

## Planning completed after baseline gate

- Framework design report done, promoted to docs/superpowers/plans/2026-09-15-measured-ui-components.md (4tasks), ledger/briefs prepared in matching .superpowers/sdd dir. Not dispatched yet. Tasks: resolvedstyle+EmptyState, RichTextLine/TextInput, opt-in Scrollable writing fill+minimalScratchpad, measuredreachableToolbar.
- Normalapp design report done, promoted to docs/superpowers/plans/2026-09-15-app-composition.md (9tasks), ledger/briefsprepared matchingSDDdir. Notdispatched. Tasks: API/DB/Code height, Containers/Merge, Store/Settings/Themes/VPN, Firewall/Windows, Chat/Backups/Feeds, Git/PDF/Snippets/Typst, TOTP, Mail/NavShell, Wordfit.
- Large-text-only apps notcoverednormalplan remainopen for corrected-capturefollowup. Do NOT confuse normalplancompletion with entireuserrequestcompletion.
- Store artwork extrafinding confirmed rootactualimages+localKurbo: positiveRect.inset expands. Added controller-addenda.md, store-3 closurefinding, Task3briefcorrective step.
- Captureworker reports18handlersmigrated, combinedcargo check18packagesPASSED. Remainingtestsignatures/scopedtests/build5pairedcaptures/commit/review. WarnedFilesLaunchSpecroutingmustpreservelegacyCLI.
- capture-after.py prepared: QOS_REVIEW_RUN outputsubdir, QOS_REVIEW_APPS commafilter, baseline4variants plus same800x900scale-baseline, PNGdimensionvalidation. Do NOT overwrite baselinecaptures.

Execution order ruling: after capture-fidelityreview, run normalappcomposition Task3 first (Store/Settings/Themes/VPN) using existingautomaticCard/Actions/FormPage; independent of pendingframeworkchanges. Then framework1–4 and othernormalapptasks. Large-textremainingfindingsstillopen. This deliversnormal-sizevisualbenefitpromptly and obeysdependencyordering.

Latest: captureworkercommit ef5f08db5, tests3064UIpassed2ignored. IndependentreviewrequiresoneImagerstandaloneenvfallbackfix; workerresumedfixround1. Freshbuild45nativewindowedand90correctedimagesfor18affectedapps completed, allrequestedPNGdimensionscorrect. Sourceef5f08db5andrecordedbaselineworkingdiff. corrected_visual_1/_2 independentlyreview9apps each(read-only); reports corrected-review-1/2.md forthcoming. No appcompositionworker yet untilcapturegatepasses.

Capture prerequisite COMPLETE after re-review: ef5f08db5 +4f0cc6b27, spec/quality PASS. App Composition Task3 worker /root/browse_composition active, BASE4f0cc6b27, most capable model high; requirements at its prepared brief. Root must review code+fresh screenshots before acceptance. Corrected visual group2 report complete and copied to durable audit; group1 still running. Read-only architecture planner /root/ui_balance_design reactivated for remaining-scale-design.md covering unassigned large-text findings and integration additions to normal app plan. Root must not stop after normal plan if marked scale defects remain.

All corrected visual reviews complete and copied to durableaudit; all18 standardroutes visuallyverified scale. Added newactualfindings toclosureJSON, closed8 explicitlynumberedcapture-onlyfindings. Remaining scale design promoted to docs/superpowers/plans/2026-09-15-app-text-geometry.md (22 bounded tasks, prepared briefs and ledger). Existing normalapp tasks1,2,4–9 amended with theirapp's confirmedscaledfindings, so sameworker fixes both aftersharedAPIlands. CurrentTask3 remainsnormalfirst; remainingTask22 handlesSettings/Themes/VPN residualscale aftersharedfixes. Same-shape task batching allowed underSDD rule but neverparallelproduction.
Fedora44ARM64 VM restored to SUSPENDED, confirmedprlctl. Resume onlywhenLinuxvalidationneeded; isolatedguestsnapshotstillavailable.
Task3 worker reports Store longdescriptionfooterregressionRED→GREEN usingCard::composed/Actions/content_sizedgrid; VPNblankheaderregressionreproduced. Root askedalignedStorepagegutters, andThemescompactpreviewonlywheneditablecustomizerpresent(presets-onlylivepreviewremainsprimary). NoTask3completionclaimyet.

Task3 interim tests Settings333/Store99/Themes49passed. VPNexistingLinux/sys/class/net/lo test fails onmacOS; preservetest, verifyVPNonLinuxlater. WorkerfixesownpointerassertiontouseexistingTorconfirmation. ThemeviewdefaultliveFullPreview/editableCustomize; explicituserchoicepreserved.

2026-09-16 continuation: original /root/browse_composition hit model capacity twice; fresh /root/browse_composition_resume (gpt-5.6-sol high) resumes uncommitted edits from task-3-resume.md. No new commits yet (HEAD4f0cc6b27). Root reviewed task3-after and task3-final captures; narrow Store STILL overlaps whole sections despite per-card fix, Themes live remains drawer-open and Settings named Region image actually Accounts. Findings recorded task3-controller-visual.md and sent to continuation. Read-only intrinsic_card_diagnosis complete: Card/ContentCard/CardGrid min-height0 likely underallocation under intrinsic Column auto tracks. New shared measured-components Task5 brief and preflight created; execute BEFORE shared1–4 after app worker saves independent edits. Task3 acceptance waits for Task5. Framework fix requires failing full/nested short viewport regression + fixedmanual compatibility, not unverified three-line patch.

Task3 continuation completed commits d3591fe85(apps)+5ca1d1d83(report). Report .superpowers/sdd/2026-09-15-app-composition/task-3-report.md. /root/browse_composition_review now read-only scoped gate; /root/intrinsic_card_fix sole production shared Task5 BASE5ca1d1d83. Both sol high after astra capacity errors. Store102/Settings334/Themes50/VPN54 pass excluding two explicit RED shareddependencytests and preexistingLinuxVPNtest. Settings route filenames MUST use underscore slugs panel-region_formats, panel-external_accounts; hyphens silently captureAccounts. Fresh truthful routes task3-resume-20260916. Pending independent review +sharedfix before any Task3 closure.

Final documentation check: ui/FORM_COMPOSITION.md still says existing apps including Store are unchanged (historical prior-task statement). Update after app/framework acceptance with verified Store example and automatic/manual boundaries; do not claim measured developer time. Existing ui/MANUAL_LAYOUT.md remains canonical manual path.

Task3 reviewer found Themes full-preview tail outside fixed560 surface/scroll extent and absent selected visual/a11y tab state; root agrees. Queue Task3 fixround1 after sharedTask5 production finishes. Linux final runner prepared linux-final-review.sh accepts immutable run directory argument, adds short/scalebaseline and VPN Linux tests; no VM action yet.

Shared Task5 candidate now GREEN per worker: three min_size.height auto changes;10composition+13legacycard tests, Store fullcatalogue andSettingsRegion formerlyRED pass. Worker still running Column/Scrollable/broadUI/build/captures; no completion/commit yet. Task3 review final task-3-review.md CHANGES REQUIRED for2Themes findings. Fixbrief task-3-fix-1.md prepared, dispatch after Task5 report/production slot free.

Task5 full UI lib3070pass; integration294pass3fail, baseline comparison requested (fails-alone is not baseline proof). Worker built isolated target at task5-after-20260916/target unnecessarily; instructed future captures reuse repo Cargo cache with isolated HOME/XDG only. Clean only newly-created task-specific build directory after evidence/report done if disk cleanup needed; preserve capture/log/provenance and repo target. No next production worker yet.

HEADcec5c10c7 shared Card min-height fix committed (plus unrelated Chromium commits736da9dc7/5673a5e67 since19f65aab7). /root/intrinsic_card_review active read-only solmedium scoped gate. Root inspected8task5PNG overlap fixed. Task5 report records3070UIlib +294integration pass,3baselineconfirmedfailures (bound_component_dispatch::legacy_bound_mutable_admission_edits_only_its_record_and_refuses_missing_or_busy; design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow; typography_visual_regression::multilingual_gallery_matches_scale_matrix). Keep these distinct from taskregressions. When review passes, markTask5complete then resume /root/browse_composition_resume with task-3-fix-1.md for twoThemesfindings (FIX_BASEcurrentHEAD), scoped re-review; then sharedTasks1–4 andremainingapps. No production workeractivewhileTask5reviewpending.
New root normalfinding settings-region-wide-balance addedclosureJSON/controlleraddenda andAppTextTask22brief: wideRegionlabelsx312 controlx932 gap excessive; use existingbounded/measured formcompositionlater, preservemanual/narrow. Do not expandTask5. closuremarkdownmayneedregenerationagain.

Task5 review: no production defect, spec NEEDS FOLLOW-UP / quality PASS WITH CONCERN because semantic Activate does not test actual Enter/Space host fallback. Fix round1 resumed /root/intrinsic_card_fix, FIX_BASEcec5c10c7, narrowly allowed ui/src/app/card_composition_tests.rs host regression + report labels. No broad suite or captures repeat. Themes fix remains queued until gatepasses.

Task5 fixround1 commit93b56dc1f; privateAppnativefallbackextract +hostEnterregression;2appcardtestspass,formatdiffclean,no sizingchange. Concurrent browser-only73d21e5fc excluded by effectivefixreviewbase73d21e5fc, verified filelist. /root/intrinsic_card_review reactivated scoped re-review, report expected task-5-fix-1-review.md. Wait gate then resumeThemesfix /root/browse_composition_resume.

SharedTask5 COMPLETE: cec5c10c7+93b56dc1f, scopedfixre-reviewPASSspec/PASSquality. /root/browse_composition_resume now sole production Themes fixround1 BASE93b56dc1f, task-3-fix-1.md requirements. Afterreport generate scoped package (exclude any unrelated concurrentbrowsercommits by path/effectivebaseverified) and resume browse_composition_review foronly2Themesfindings/newbreakage. Then Task3completion and sharedTasks1–4 sequentially. Allother appplans stillpending.

Closure update after independentappreview+sharedTask5PASS: closed Store footer/header/artwork/filter findings and original inconsistent-scale screenshot finding (truthful2× now enlarges search/filter/actions coherently), Settings preamble, VPN header. Store allmarkedfindingsclosed pendingfinalmatrix; Settings normalwidebalance/scaled and VPNEmptyState remainopen. Fixed duplicate Store id: artwork now store-artwork-inset, originalstore-3 retains scale issue. closuremarkdownneedsregenerationforlastStorechange.

Task3 Themes fixround1 commits f3e6e6f7f/711191161/report7231a8170; /root/browse_composition_review active scoped re-review via review-203032223-7231a8170.md. Excluded verified external203032223 Chromiumtransport/UIwebview+manifests. Root visuallyinspectedall7postgutterThemesPNGs, PASSselectedstate/previewcontainment+reachableshorttail; notes task3-fix-1-controller-visual.md. Once reviewerpasses markAppTask3complete, closeThemesnormalfinding, dispatchsharedTask1 next freshsolhigh. FrameworkTasks1–4 andallremainingappplansNOTSTARTED. Ownedcommitmanifest at .superpowers/app-ui-2026-09-15/owned-commits.json tracksours/external; updatefuturecommits. Plans+preparedbriefsnowexplicitreuseCargo cache andisolateHOME/allXDG forruntimecaptures.

AppCompositionTask3 COMPLETE: finalThemefixreviewPASSspec/PASSquality, closesThemesnormalfinding. /root/resolved_text_empty_state sole production active sharedTask1(solhigh) BASE7231a8170, requirements preparedtask-1-brief.md, reportexpectedtask-1-report.md. Aftercompletion scoped review then shared2/3/4; shared5alreadycomplete. Useroveralltaskfarfromcomplete: allothernormalapp+22scale/geometrytasks pending, final48appmatrix/UXbench/Linux/finalreviewpending. No finalpartialresponse.

Task1 initial14text_scale+1theme_resolved gatespassed; RED testsadded. Root approved retainedtiny-pane design: autoheight/min-height100% +into_scrollable existingScrollable route; immediatepublicminimum_essential_height and no whollyinvisiblehittarget. Needactionvisible/realkeyboard+pointerafter scroll, normalcentering/manualcompat. Task1 stillactive /root/resolved_text_empty_state. Added snapshot-linux.py helper forimmutabletar/hashes(runnamearg); NOTRUN, noVMaction; archive hashes exactsamebytes, excludes.superpowers/target. FinalLinuxrunneralreadyprepared requires same runname.

Comparison preview validated via CUA: Earlier/Verified toggle and app selector update screenshot+accessible caption; 320px wraps controls and screenshot, default wide displays actual PNG. Preview tab closed, viewport reset, localhost8768 server stopped. Fragment in thread visualization dir app-layout-comparison.html must be emitted in final. Capture-after script now also isolates XDG_STATE_HOME.

Root corrected two stale statements in ui/FORM_COMPOSITION.md (Store adoption and already-existing inspection integration); docs diff only, verified source anchors. Uncommitted owned doc, include final review/commit. Task1 provisional root PNG review in task1-controller-visual.md confirms EmptyState improvement; VPN top Tor capsule still too short at2x, retain Task22 residual.

SharedTask1 complete17110d403 + task-1-review.md specPASS/qualityAPPROVED. SharedTask2 dispatched/root/editor_measurement solhigh BASE0e1ad3bb6; sharedTask3/4thenremainingappworkpending. RootdocsFORM_COMPOSITION.md remainsuncommittedowned. Comparisonpreviewvalidatedandserverstopped.

Closure register now maps all48apps to explicit shared/composition/text-geometry task ownership or finalverification; nofindingsclosedbythisdocumentationpass. Renderer includes Assignedworkcolumn and planlinks. Needrootappverification Notes/Calendar afterShared2; ScratchpadP2hierarchy reevaluateafteractualeditorfill Shared3.

Rootdocsowneduncommitted: ui/HIG.md §7a mapsacceptedexistingform/card/actions/EmptyState/Manual APIs toregionintent, primaryremainingworkspace/grouping/disclosure, same-styletextnormalization andscrolling, explicitmanualchoices. ui/FORM_COMPOSITION.md staleinspection/Storeclaims corrected. SourceAPIanchorschecked; diffcheckclean. Include in finalscopedreview/commit; addacceptededitorfill/Toolbar guidanceafterlanding.

Closureupdate afterShared1review: vpn-1oversizedemptytext CLOSED with17110d403+review+freshPNG; newlyexplicit vpn-header-control-height remainsOPEN, TextGeometry22. NoallVPNpassclaimed.

Root documentation committed e6bdeb72f: only ui/HIG.md and ui/FORM_COMPOSITION.md. Diff check passed; claims verified against accepted source APIs. Include in final scoped review. No root production source edits; Task2 worker notified.

Prepared Linux verification script corrected before use: fresh guest source directory required, capture parent directories created, all HOME/XDG paths isolated, dark theme explicit, PNG signature/dimensions/hash recorded and aggregate failures return nonzero. Shell + embedded Python syntax checked; VM remains suspended and no Linux final run performed.

Task2 all15freshPNGs inspected. Notes residual ordinary-size styled glyph overdraw marked, read-only rich_mark_diagnosis active. Calendar scaledchip/NOTESseparator residual marked and new TextGeometryTask23 added; brief preparation pending. Typst gutter improved; toolbar/status remainComposition6. Task2 fixround1 active, both independent review Important findings accepted.

Capture-after tool hardened for future evidence: rejects reuse of existing capture-results run, validates PNG signature/dimensions and hashes each image; optionalQOS_REVIEW_SHORT=1 adds800x480, failures returnnonzero. Syntaxchecked; existingcapturesunchanged. Verifiedexternal59ffa17fe/7e90bcf68 browseronlyexcludedfromownedmanifest.

New sharedTask6 added withbrief for Notes markoverdraw: read-onlyrich_mark_diagnosis confirmed duplicate plain+styledglyphs and plaincaretgeometry; existing RichTextLayout::shape_styled is sufficient. Preservecodefaceappearance, glyphsonce+samecaret/linkshape, style-only/IME/historyinvalidations. ExecuteafterTasks3/4. TextGeometryTask23Calendar brief nowprepared. Existingallappobjectiveunchanged.

Root durableaudit/spec/plans committed754afd005, exact15docspathsverified; no sourcechanges. Externalf10c8dfc3/b7c998714 verified guidedImagersetup source/docs, excludedownedmanifest. Future Imager pass mustpreserve newlylandedsetupflow. Task2fixround1stillactive, noacceptanceclaim.

SharedTask2 COMPLETE3f13fb4eb+83c269dac; scopedre-reviewPASSallfindings. Root9freshpostfixPNGs individuallyinspected. Shared3nowready, then4then6; AppCompositionremaining1,2,4–9 andTextGeometry1–23stillpending. Do notstopatsharedfixes.

Task3soleproduction/root/writing_surface gpt6astrahigh startedBASE83c269dac. Requires fullwritingviewportoptin, intrinsiclongoverflow/manualfixed/defaultcompat, lowerclickcaret, Scratchpadsave/reopen. Task4/6thenallremainingapps.

Accepted report/review copies promoted to durableaudit implementation/ directory (15files+index), READMElinksupdated; uncommitteddocsowned. Keep .superpowersworkspaces untilwholeplanreview/cleanup. Task3activeRED2fails confirms36pxinput vs360footer; integrationtestplacementaccepted.

Shared3 integrationcaught firstwheel swallowed by subpixelTextInputoverflow; scopeexpanded narrowly to intrinsicheightcontainment withrealfirstwheeltest. Worker mustnotmerelyrepeatwheelinharness. v2Scratchpadsupplementswide1000x700/narrow480x600 lookedclear; rootwillcapturestandardmatrixafterfinalbuild. Shared3notcomplete.

Verifiedexternal2fdba6dbc guidedsetup+headlesskeyboardhelper,62f7a9b25 browserdailycontrols; excludedmanifest, noShared3sourceoverlap. Shared3firstwheelregressionbeingmadefractionaldeterministic; original47row18fonttestroundedup,18.1fontcandidate deliberatelyroundsdown. NoTextInputproductionfixuntilbehaviorRED. Currentexternalnative_scenario.rs App::native_input_snapshot missingblocksUIlibtests; acceptedintegrationroutepreservescoverage, finalbroadsuitestillpending.

Shared3candidatefba9ea50f reviewpendingwriting_surface_review; allfocusedchecksGREENincludingactualfirstwheel+repeatclickfix. Root6standardScratchpadPNGs+4finallongbottominspected. Task3harnesscaptures-final liveINSIDESDDworkspace: promote images/logs/harness/patchbefore eventualworkspacecleanup. Readonlytrailing_line_diagnosis checksseparatewhole-rowtrailingnewline discrepancy; notTask3fixscope.

Shared3 COMPLETEfba9ea50f+untrackedScratchpad1line, independentlyApproved androotstandard6+longbottom4imagesPASS. CloseScratchpad4markedfindingswithreport/PNG/testevidence; hierarchyresolvedbyprimaryregionexpansion, manualsavehintretained. NextShared4Toolbar, thenShared6richmarks, remainingAppComposition/TextGeometry. Trailingnewlineindependentdiagnosisstillrunning.

Shared4soleproduction/root/measured_toolbar astrhigh dispatchedBASEfba9ea50f for actualmeasuredToolbar+boundedreachableContextMenuoverflow. Shared6thenremainingappplanspending. RootacceptedScratchpadfindingsclosed4; comparisonfragmentshouldaddScratchpadafterreloadvisualizeskill.

Ruling: confirmed trailing-LF attributed shaping defect becomes Shared Task7, after4 before6, isolated richtext boundary+TextInput tests. Task6 consumes repaired shape_styled; no simultaneous source writers. Preflight:7↔2/3 same TextInput tests, preserve accepted measurement/ceil/focus;7↔6 same RichTextLayout dependency, sequence7first;7↔4 no shared production file;7 self-check tests match final empty row/attrs contract, no LF-decoration redesign. Cost if wrong: revise normalization, never mask with shorter measurement. Diagnosis has runtime proof; Scratchpad new finding remains open, its four prior findings stay closed.

Comparison now has five app pairs, Scratchpad selection and Earlier toggle verified through CUA; preview regenerated with --force, temporary tab/server closed.

Root docs commit0e103c4d7 exact23ownedpathsverified. Task3acceptedcapture/log/harness/patch archiveoutsideSDD at task3-accepted-evidence with55filehashmanifest; durable reportexplains historicalpathmapping. Readonlyediting_workspace_preflight active forComposition1; secondread-onlybatchpreflight dispatch/refollowuprejectedbyagentthreadlimit, no workstarted. Shared4continues soleproduction. Closure90findings=21closed69open.

Concurrentcommits6f823cf11/b0768af20/ebdef45f6 verified filelists: internalinstallerdocs + Imagerguidedsetupcontroller/view +verificationdocs; excludedownedmanifest. FutureImagerTextGeometry12mustpreserve newlyacceptedguidedsetup. NoToolbar/TextInputoverlap.

Ruling: sourcepreflight splits umbrellaTask1 into sequential10CodeDock/11API/12DBsplit/13CodeMetrics/14DBMetrics; Task1 completion requires all5, do not dispatch original Task1. Preserve existing semantics rather than invented Params/Auth editors, DB row selection or Code manual dock resizing. Add visible retained Code toggle because collapsed dock otherwise loses pointer entry. Source-only preflight editing-workspace-preflight.md; no app changes/tests yet.


Remainingappcomposition executionnow10–14plus2,4–9;3complete. Original1isumbrellanotadditionalwork.

Shared4candidate1b3eac1b4exact3pathsverified; independent/root/measured_toolbar_review astrhigh active via review-task4-1b3eac1b4.md. OriginalBASEfba9ea50f effectiveBASEd02c1ade3 after verifiedinterveningpathhistory (onlyTask4changes toolbar/context_menu). Root2fixturePNGspass; focusedworkerchecksallgreen; noTask4closureyet. NextproductionShared7then6, thenremainingappplans.

Task4fixround1 activeBASE2aff2e6c7822276508c60d7e414aee212141a3df; review has3Important (exportedbounds, stalehoverwrongtarget, nonleftstuckcapture). Rootcheckedactualeventbranches+AccessKitbounds, acceptsall. Brief task-4-fix-1.md; sameworkerresume, scopedre-reviewafter. No otherproductionworker.

Shared4fixround1c7b2d576b committedexactToolbar+report; re-reviewactive measured_toolbar_review. Use review-task4-fix1-scoped-c7b2d576b.md ONLY (effectivec66ec996c excludesverifiedImagercommit); unscopedpackagecontainsunrelatedcommit. RED0/3→GREEN3/3,Toolbar18/menu15/layout7/checkformatpass; no paint/layoutchange,capturescarried. NextShared7then6 onlyaftergate.

Task 4: complete —1b3eac1b4+c7b2d576b; task-4-fix-1-review.md all3findingsADDRESSED/specPASS/qualityApproved, nonewissues. Rootnormal+narrow2×fixturespass (archivedtask4-candidate-captures). Onlyknownbaseline__eh_framewarning, liveOSaccessibilitynotclaimed. Shared7soleproduction/root/trailing_empty_line_fix solhigh BASEc7b2d576b dispatched; thenShared6, thenallremainingappwork.

Rootdocs8a678e12c acceptedToolbarHIGrow/lifecycleguidance, updatedappplan+15preparedbriefdependencytexts, durableTask4report/reviews; exact6commitpathsverified. FixtureimagesalreadyarchivedoutsideSDD. Shared7active; noappconsumerfindingclosedbyToolbaralone.

Linuxstateupdate readonlyprlctl2026-09-16: Fedora44ARM64{dc784719-37f7-4f95-bd93-8557fa682d46} isnowRUNNING, despitebeingSUSPENDEDafterourearlierwork. Thisrootdidnotresumeit; concurrentinstaller/platformworkexists. Ruling: doNOTblindlysuspendatfinalfromoldstate. RecordfreshstateimmediatelybeforeourLinuxrun, preserveotherwork, restoreonlypowertransitionthisrootowns. NoVMmutationmadeinthischeck. FinalLinuxstillpending.

Capturetoolprovenancehardened: recordbinarySHAafterallvariantsandfailbatchifbinarychangedbetweenstart/end (concurrentappbuilds). Pythoncompilecheckpassed; nextrealcapturewillverifyordinaryroute. Noexistingcapturemodified. Task7focused3regressionsRED→GREEN; requestedaffectedexisting suitespending.

Shared7candidatefd7b0964f exact3ownedpaths verified (richtext production/TextInput regressions/report), independenttrailing_empty_line_review solhigh active via review-task7-fd7b0964f.md. OriginalBASEc7b2d576b effectiveca5b92f36 excludesverifiedexternalbrowser/docs; onlytaskcommit touchedownedpaths. richtext20/TextInput100/CodeEditor23/Scrollable12/check/format/new3postformatpass. Reportlabelsadditionalmutationchecksassourcereasoning, not executed. NextShared6afterreviewgate.

Task 7: complete —fd7b0964f, independenttask-7-review.md specPASS/qualityApproved/no findings. Scopedrichtext20/TextInput100/CodeEditor23/Scrollable12/check/format/new3postformatGREEN. ClosesScratchpadfinal-empty-line runtimefinding; original4closedstayclosed, finalappmatrixpending. Shared6soleproduction/root/single_shape_rich_marks astrhigh BASE90e68a2e9 active; briefwithaccepted7dependency, preserveitsnormalization. RootagainviewedNoteswide+2×task2-fix1PNGs, boldoverdrawconfirmed.

Task6checkpoint: duplicate-glyph/mark-onlygeometryREDtestsbuilding; designstaysRichTextLine/TextInput, existingRichTextLayout+draw_rich sufficient. Rootreinforcedchipbackgroundmustpreserveselection/presencelayering, notcoverwithopaquechip. No production scopeextension. Rootdocsaccepted7commitcbfa523b7 exact6ownedpaths verified.

Task6firstGREEN: glyphcount3vsold4; mark-onlyboldtogglechangescaret/selection1×/2×. ChipprepassbeforeTextInputselection; BlockEditorpeerpresenceafterlinespreserved. Furthermeasure-before-draw/UTF8IME/fullsignature/modelreplaytests+actualNotesinteraction/capturespending. ExistingNotes--shotsroutesavailable; noappsourceextensionapproved.

Task6Ruling: approve narrowblock_editor.rs current-frame link-hit refresh/test. Rootverifiedrefresh_live_link_hits atdrawstart3838 vsdraw_blockloop3925; peerpresencealreadyafterloopforfreshcharpositions. MovinglinkrefreshafterdrawnecessaryforactualNoteslinkacceptance; firstdrawpreviouslyempty, style/scalepreviousframeedge. Ownedfilecleanbeforeaddition. RequireRED/GREENfirstdraw+style/scaleedge, nootherBlockEditor/appredesign. Costifwrong: adjustphaseplacement/narrowinvalidation, notstalehitapproximation. Brief/planupdated.

Task6transientexternalcfgtestblocker: Chromiumtests NavigationDirection missing/NavigationHistory+RequestNavigationHistory directionfieldsabsent. EarlierownfocusedglyphGREEN/measurementUTF8REDcompiled. RootinstructedpreserveChromium, continueindependentsource/library/appcapturework, retryonceexternalsource settles; noisolatedtarget/blindloops. Task6gateremainspending.

Task6blockerclearedononere try (Chromiumownerresolved; noownedchanges). ActualBlockEditorlinkREDfirstdrawNone→GREENrefreshaftercurrentdraw/presence. Attributed5testsGREENmeasureweights400/700/400at1×/2×,UTF8IMEeventcommit,chip/linkgeometry,fullstyles,actualCRDTsync/undo/redo. Notesnormalbuildsucceeded; workerstandaloneNotesAppharnessinevidencedir(noappsourceedits) checkingcaret/dragcodeselection/boldlinkedge navigation1×/2×. Rootownsstandardcapturematrixafterfinalbinaryhash; workerownsinteractioncaptures.

Task6 ruling: use existing TextInput selection rectangles for BlockEditor link hit bands, preserving the full-line vertical target. Worker self-review found that two caret endpoints still fail reversed/mixed-direction ranges. This is within the approved live-link scope and authoritative-shaped-range requirement; no new API. Require a focused regression for actual segments and exclusion of unrelated gaps. If the change is wrong, revise the range mapping rather than retain endpoint approximation.

Task6 controller inspected all6 standard Notes captures plus10 actual interaction captures individually. Candidate visualPASS; task6-controller-visual.md. Capturer exercised new binary-stability guard successfully; final worker rebuild hash identical6b5d2cbbb5ed9f4b582606b0aa25686edca09015941e44707f9e8aba7fd45c97. Actual caret/selection/code and bold-link1×/2× harness GREEN, affectedmodules265testsGREEN; source review pending. Existing workspace Clippy diagnostics noted by worker, no broad fixes authorized. No finding closed before review.

Concurrent ownership checkpoint: unrelated dirty Word actions.rs/widget.rs (text-edit materialization/performance), NavShell and Tabs remain present. Before Composition9/8 or any related task, recheck fresh source status/owned paths and preserve these edits; do not whole-file commit concurrent uncommitted changes. Current future app task10 Code Editor paths remain separate. Newly excluded concurrent commits e308647ff/3b1f1243f/ff01d3ba5/fe4908e59 file lists inspected and added manifest.

Task6 candidate73dde03c6 exact4paths verified; independent/root/single_shape_review astrahigh active via review-task6-73dde03c6.md. FinalNotesbinaryc1f09b8aede7e4a2021a9f0704818a13d0ae739dda8ba782d722bfd8eb655039; task6-notes-final-20260916 all6PNGsbyteidenticaltoindividuallyinspectedearliermatrix. Interactionharnessrerunworkerbyteidentical. NextproductionComposition10afterTask6gate, then11–14 and remainingtasks.

Shared6 accepted73dde03c6 aftertask-6-review.mdPASSspec/Approved; root resolvesvisual/hashcaveatswithfreshfinalmatrix and source/PNGhashchecks. Notes2findingsclosed (90total/24closed/66open). Rootdurabledocsdd4538234 exact7filesverified. Currentsoleproduction/root/code_dock_composition solhigh BASE73dde03c6, task10brief; sharedwholeplanreadonly/root/measured_ui_final_review astrahigh active with final-owned-review.md tenownedcommits300734bytes. No sharedfixworker until CodeDockproductiongate or interruption if needed.

Comparison updated sixthappNotesnarrow using individuallyreviewedbefore/afterPNG;12actualimages869044bytes. CUA verified Notesselect+Earlier/Verifiedtoggles and pixels;tempnotesCompareTabclosed,server68398stopped. Fragmentmustbeincludedin eventualfinalsame-turncontentreference;fullvisualizeskillreloadbeforeanynextedit. Allappworknotcomplete.

Sharedfinalreview linttriage:100existingerrors is relative toTask6; somecome from earlierownedTasks1–4. Do not call allunrelatedbaseline in finalsummary. Reviewer categorizingminor; nofunctionalblockerreportedsofar. Actualproductionfix if required must wait soleworkercoordination.

Sharedwholeplanreview complete: final-review.mdApprovedwithminorlintdebt, noCritical/Important; durableimplementation/shared-final-review.md archived. No allappcompletionclaim. CodeDockTask10soleproductionongoing.

Linux final capture script now mirrors macOS binary-stability guard: hash before/after each app's variants, record both/stable, fail if concurrent build changes executable. bash -n and both embeddedPythonblockscompilePASS; runtime remains notrun. FreshVMstate must be checked before ownrun; priorread foundRUNNING fromotherwork, not ours to suspend.

Task10checkpoint: defaultcollapseRED→GREEN and retainedtriggerinteractionGREEN480×900. Same44pxheaderrow/disjoint40pxtrailingbutton, ToggleButtonselected/checked/expanded; pointer/Enter/Ctrl+J/Escape+focusreturn pass. Existingtests/build/namedroutecapturespending; notaccepted. SharedSDDworkspace immutablearchiveoutsideSDD at shared-plan-accepted-20260916/workspace.tar.gz (2289821bytes, manifestSHA); durable reports alreadytracked. Preserveuntilfinalauditcleanup.

After interruption, sole CodeDock worker was interrupted and resumed via followup_task with completed tests preserved. Task10 remains uncommitted and unreviewed; report absent at resume. Pending up/down icon feedback, named captures/final hash/report/commit. External c2242f9aa and ac81de14e file lists checked and excluded (Imager docs, Browser tabs/media/webview). No root production edits.

Task10 e7a4ec846 source+evidence scopedcommit verified. Independent code_dock_review solhigh active; sourcepacket40KB excludes bulky repeatedretainedtreeJSON. Root6finaldefault+6namedroutes PASSnormalcomposition; expectedbinary3be37c... stable and hasheschecked. Up/downchevronsreplacegrid. 206/207libpathaliasfailure notobservedbaseline; honestreport. No Task10 closure yet. Next soleproduction Task11API after gate.

Task10 fixround1 f3a27b85c committed2ownedpaths; scopedre-reviewactive code_dock_review. RealCtrl+JtestREDfocusedCodeeditorinsteadShowbottompanel; GREENcanonicalclosehelperclearsterminalfocus+existingfocus_targetroute. Fivefocusedtests+fmt/diffPASS; sameinstance narrow/wideresize/liveTerminalobject+gridpreserved. No paint/layoutchange, prior12imagescarried. NextTask11aftergate.

Task10 accepted after f3a27b85c re-review bothfindingsADDRESSED/nonewbreakage. Durable3reports and closure committed ea895337a (6exactfilesverified). Only code_editor-1 closed; register90findings/25closed/65open,48apps, allclosedhaveevidence. Task13scalededitor remainsopen;Task1umbrella incomplete. Sole production /root/api_composition astrahigh Task11 BASEf3a27b85c dispatched, clean API paths, brief includes acceptedshareddependencies and capture responsibilities. NextTask12DBsplit,13CodeMetrics,14DBMetrics, thenremaining2/4–9 andallTextGeometry1–23.

While Task11 API is sole production, read-only /root/code_metrics_preflight solhigh is preparing upcomingTask13 technicalgeometry map; output code-metrics-preflight.md, no edits/tests/builds/subagents. Root independently inspected DBrelayout+accessibility; appended specificTask12anchors to editing-workspace-preflight.md. Task12 plan/brief staleA1reference clarified explicitly requestedmanualheightvsclamp/Reset; tracked app-composition plan change awaits next docscommit. Root also re-inspected API baselinewide/compact/narrow and recorded task11-controller-visual.md; no newfindings/closure.

### Controller scope ruling — 2026-09-16

True 2× captures confirmed ToggleGroup passes resolved typography into a renderer that scales again. The sole Task 11 worker may normalize that shared draw/measure style with renderer_input_style, with focused 1×/2× regression and existing ToggleGroup tests. Keep this dependency narrowly scoped and independently reviewed; no padding or general sizing changes.

Saved-request loading currently loses local method/body/URL during document reconciliation. Preserve and report that pre-existing limitation in this composition task; Body fixtures must identify how they were staged. Do not claim saved-request content loading verified. Track canonical loading repair separately.

### Controller preflight ruling — 2026-09-16

Use one editor metric snapshot for existing paint, hit testing, caret, selection, scrolling and overlays. Preserve host UI/text scale when selecting an app palette; invalidate parent structure when changing panel height class. Preserve Task 10 disclosure/focus behavior and existing fixed outer dock heights.

This is metric repair, not new editor capability: do not add IME handling, ime_cursor_rect, or synthetic line accessibility children. Preserve existing Document/control semantics and bounds; report existing IME and character-cell shaping limitations honestly. Do not reuse experimental cba6d9c75. Source evidence: .superpowers/app-ui-2026-09-15/code-metrics-preflight.md.

### Task 14 controller source rulings — 2026-09-16

- Keep stored logical row/x preferences across temporary metric/viewport clamps; use bounded effective offsets consistently in paint, hits and scrollbar. A new explicit user scroll starts from the effective position and updates the preference. New-result bind keeps its existing reset semantics. This satisfies scale restoration without painting an out-of-range thumb.
- Grid pixel-wheel conversion belongs to metric integration: verify actual event units through the host path, normalize physical pixels once, and convert using measured row geometry with retained fractional motion if necessary. Keep existing line-wheel behavior and do not change sidebar scrolling.
- Include measured widths/bands and collision handling for existing tab, Run, Snippets, result/count controls; snippet paint and hit rectangles must agree. No new snippet keyboard/a11y feature or wholesale Toolbar migration. Structure consumes the shared tab origin only; do not refactor its rows or the sidebar.
- No selected-row model, column resizing, interactive scrollbar or grid row-number gutter exists; do not introduce them. Repair bounded half-open header/body hits and width/paint agreement within GridMetrics. Task 14 consumes accepted Task 12 geometry and requested-height state.

Ruling: these narrow dependencies make the specified scale/scroll/target contract coherent; if wrong, revise the local metric/state seam without changing documents or adding controls. Source preflight: .superpowers/app-ui-2026-09-15/db-metrics-preflight.md.

## Fix 1 lifetime dependency ruling

Serial and parallel affected a11y runs SIGSEGV after focused saved-row activation / collection toggle. Worker macOS stacks localize access to NavRow accessible_name via CompositionFocus::capture. Root verified rebuild_nav replaces/drops Vec immediately; headless captures focus before sync, live App also reads old focus before rebuilding. Both host paths clear bindings/node_map before root.build_layout (headless1043–1091; app6031/6103).

Ruling: authorize app-local retired Vec<NavRow> generations until the beginning of build_layout, after the host discards old maps. Every replacement must mark topology dirty even if the child count/signature is unchanged. Preserve current visible inventory and action identity; no shared unsafe/lifecycle patch. Cover focused same-count replacement and bounded reclamation, then previously crashing host interactions and affected a11y. Cost if wrong: revise the local lifecycle boundary before acceptance; do not suppress the crash or downgrade it to test noise. This dependency is necessary to verify current UI interactions.

Task 11 fix round 1 candidate: 21b43ddb6, exactly 43 owned paths verified (two production files, report, fresh evidence). Independent re-review active on api_composition_review using review-task11-fix1-source-21b43ddb6.md. Original fix base 04b08caa2; effective base cb193f982 excludes verified concurrent controller/Imager docs and source. Final 72 library + 13 a11y pass; actual earlier parallel and serial SIGSEGV preserved and resolved by authorized row lifetime repair. Root individually inspected final 12 named + 6 default PNGs, verified current source/24 named evidence hashes and stable normal binary6cd8cb6d. No closure until review; Task12 still next.

Task 11: complete — 6518c7024 + 04b08caa2 + 21b43ddb6. task-11-fix-1-review.md: wrapping/alignment addressed, app-local lifetime repair accepted, spec PASS / quality Approved, no new blocking breakage. Root final18PNG visualPASS and exact source/binary/evidence provenance verified. 72lib +13a11y finalPASS. Shared original linker warning retained; separate canonical saved-draft adoption issue remains explicitly OPEN in discovered-followups.md and api-draft-preflight.md. This closes composition/scale findings only, not that functional follow-up. Task1umbrella remains incomplete.

Task12 next sole production: DB Client automatic/manual query pane. BASE7ce424b9e, task-12-brief.md. Owned app.rs/session.rs/grid.rs clean at dispatch. Root baseline wide/compact/narrow inspected; split pass uses shared editor line metrics and leaves grid/chrome scale toTask14.

Controller docs df0572faf archived accepted API reports/reviews and separate open draft preflight; exact9paths verified. Closure90/28closed/62open. Restored original Unicode JSON serialization immediately afterward (uncommitted formatting-only restoration for next docs commit); retain ensure_ascii=False in future register writes.

Controller09e32238e exact3docs: scopes Task15 API canonical draft/route repair (not started), preserves open status and restores Unicode serialization with semantic JSON equality verified. API comparison updated and browser toggle/screenshots verified; temporary tab/server closed. Concurrent Browser/Imager1b8ade978/dbb79c55a/d17e5d6d8 excluded. Task12 tests briefly hit concurrent WebView enum exhaustiveness; owner supplied match arms before root intervention; worker reruns, no shared edits from this task.

Ruling: textTask9 verifies concurrent Files/NavShell repairs before new changes; exact relevant source hashes match external UX-bench manifest64f60193 at inspection. Preserve dirty ownership; only residual acceptance gaps justify edits. Costs if wrong: targeted remaining-state repairs. Evidence external-ux-evidence.md; no all-app closure or fresh controller bench run claimed.

Root prepared Linux validator inspection found the previous stability guard reused `digest` for PNGs, falsely comparing the last PNG hash with the executable. Corrected task-owned linux-final-review.sh to distinct binary_digest/png_digest. bash-n passed and extracted Python guard was executed with synthetic subprocess/header fixtures:2apps×6variants accepts unchanged binaries and rejects changed binaries. Evidence linux-capture-guard-verification.json. No VM/runtime capture claim; final Linux still pending. Plan Task4 stale “restore suspended” wording corrected to fresh-state/owned-transition rule; readonlymanagementpreflight informed.

Task12 e58c94cf3 exact9ownedpaths, final79libPASS, source/binary/8artifacthashes verified. Root6defaults+4named finalvisualPASS but independentreviewNeedsfix: openmenu consumesRunshortcut and staleanchorafterresize. Soleworkerdb_split_composition fixing round1, task-12-fix-1.md; also resolves manyconnections and actualclampedlongquery acceptance. NoTask12closure orTask13implementation yet. Readonlymanagement_disclosure_preflight active independently.

Task13 priorpathfailure now reproduced oncleanacceptedCodeEditorpaths beforeTask13: /var vs/private/var exactcomparison; logtask13-preexisting-path-test.log, commandexit101. PreserveasTask13baseline, notoriginproof. Briefupdated.
