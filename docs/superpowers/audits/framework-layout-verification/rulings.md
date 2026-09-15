# Recorded implementation rulings

Chronological record from the controller ledger; superseded choices are retained so the decision history is visible.

1. user execution request approves the existing architecture and incremental implementation — no extra design-permission round — user reviews concrete changes after screenshots.

2. user wants a review ping after every change — do not batch behavioral tasks; notify with artifacts and allow review before the next mutation — independent preparation may continue.

3. no Store file changes, including tests or capture code — demonstrate framework changes through existing Store path and framework-owned probes — app-specific artwork sign bug remains outside scope.

4. preserve unrelated dirty files and use exact path staging — concurrent macOS/filesystem work is not ours — baseline may move so validate source hashes.

5. skill scripts are readable but non-executable — invoke through bash with explicit output paths where nested execution would fail.

6. review package filters recorded base..task commit to task-owned Column/Card paths — concurrent filesystem commits are outside this task — separate final review must retain this ownership distinction.

7. expand Task1 ownership to Scrollable implementation only if required to scope non-stretch behavior to scroll content — preserving normal parent alignment is a binding manual-layout constraint — regression risk covered by Row, Column, and Scrollable tests.

8. Task2 may add a shared bounded post-compute style-adjustment hook in Widget + App/headless if native Grid cannot preserve below-min-width fitting — Taffy0.10.1 calc resolver is unusable for min(100%,configured width), and correct host geometry is binding — broader lifecycle surface requires focused identity/settling tests. Existing raw Store capture must remain correct at ordinary widths using native auto-fill default; no Store edits. Task3 consumes this helper, not a duplicate loop.

9. Task3 includes the concrete VirtualList retained-node-reset defect exposed during Task2 — resize/rebuild must preserve actual row geometry — scope includes virtual_list.rs regression/fix, preserving fresh-composition sync requirements.

10. Task3 may extend TextInput freeze/thaw state backward-compatibly to include selection anchor — current caret-only state cannot satisfy explicit selection preservation across freshly composed controls — cover old serialized state and real resize selection.

11. Task5 may include focused shared Button intrinsic sizing/centering support needed by automatic cards/actions — new patterns cannot honestly claim200% containment otherwise — preserve existing explicitly fixed contracts and independently review affected Button regressions/captures.

12. the spec requires real measurement/placement of standard controls and composed form/container children, including mixed nesting, but does not require rewriting every legacy geometry-dependent app shell to predict multiple alternative topologies in one callback. Task7 must accurately document current-retained-subtree measurement and settle the chosen allocation, test width-dependent Text/Form/containers, and avoid claiming a universal pure application-layout predictor. Stronger alternate-AppShell multi-proposal support is a possible follow-up, not a new mandatory whole-framework rewrite introduced by preparation. Costs: some legacy topology-dependent children cannot answer hypothetical alternate layouts synchronously; this limitation must be explicit. Exact logical placement, safe ownership, and overflow correctness remain required.

13. fit_content compact width may settle measured width through existing compute_widget_layout when native Taffy Dimension cannot express max-content; preserve actual App/FrameHarness alignment and sizing. Direct raw compute_layout_with_measure bypasses framework style settlement, already needed for narrow/automatic grids. Do not add guessed metrics/private renderers/new wrapper trees to preserve that obsolete capture path. Cost: unchanged Store --shots raw loop can still show stretched fit-content controls; retain and explicitly label that limitation, use shared public harness captures as authoritative actual-host evidence. No Store source changes permitted. Final audit must disclose this boundary rather than claiming raw path fixed.

14. Task7 may add missing vello_shaders generated manifest patch beside existing winit/vello_encoding/vello patches. Actual external scaffold cargo test fails resolution in11-scaffold-red.log because SDK localVello0.9 needs unpublishedshaders supplied byworkspacepatch. Direct prerequisite for mandated compiling scaffold; addfocusedregression+actualappcheck. No rootCargo/Store edits. Cost: generated localSDK manifests gain one existing workspacevendor path.

15. documentexactplatformfailure/unchangedcode, do notmodifylocale logic; noextrabroadrerun needed afterfocusedstarterfix. Fullui-lib22-full-ui-tests.logrunning.

16. include concrete default NumberInput 200% failure exposed by generated starter in final acceptance fixes — automatic starter must render its existing controls pleasantly without authored sizes. Preserve explicit with_size; no general control sweep. Cost if wrong: a bounded default-control sizing change requires additional compatibility tests/captures.

17. approve cloneable ManualInvalidation handle returned by Manual::invalidation(); authors change captured input, invalidate, and request a layout frame through normal host scheduling. Dirty policy participates in layout_needs_measure and successful solve acknowledges only solved work; failed solves retain dirtiness. This preserves retained children and avoids recomputing unchanged scroll frames. Cost if wrong: authors need one explicit invalidation call; arbitrary closure captures cannot be observed automatically. Require actual painted same-width geometry/hit/a11y/focus regression.

18. NumberInput uses explicit-size tracking; automatic default footprint scales with theme text scale, while with_size and existing minimum-target semantics remain manual. Center stepper glyphs using actual renderer-measured height. Concrete generated starter340px at200% has292px available and fits scaled280px default. This fixes demonstrated default failure, not arbitrary suffix intrinsic sizing. Cost if wrong: default NumberInput takes more room under larger text; cover layout, editing and glyph containment.

19. revise NumberInput default-width correction after RED proves even100% default inner68px is smaller than measured15min+padding68.3125px. Reject fixture-tuned140→144; use actual formatted value measurement plus existing padding/stepper/gaps as an automatic minimum, retaining current140*scale baseline and explicit-size semantics. Reuse existing renderer-aware settlement hooks. Cost if wrong: default numeric control width can vary with displayed value; test retained updates and compatibility.
