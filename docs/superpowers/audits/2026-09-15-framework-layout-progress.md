# Framework layout implementation progress

Status: the approved seven-task implementation is complete through `7bebe26b6`. Store source remains unchanged. The sections preserve per-change verification and limitations.

## Change 1: reliable intrinsic column gaps

Commits: `f14fdb98f`, `050c5653e`, `bab974dc8`.

Intrinsic Columns now use a single-column Grid with natural row sizes and explicit gaps. The host tree retains exactly one node per widget child. Vertical Scrollable owns the non-stretch behavior needed for natural content growth; normal Row centering and horizontal scroll stretching remain intact. Framework Card title/body spacing remains 12 pixels after removing its spacer workaround.

Two independent review/integration findings were corrected before proceeding: a child alignment override initially broke Row centering, and a stretched parent initially expanded implicit grid rows and inflated effective gaps. Tests now cover both configurations.

Verification: 15 Column tests pass, including literal gap, margin, padding, nesting, parent-alignment, horizontal-scroll, and vertical-scroll expectations. Card 11/11 and Scrollable 33/33 regressions pass; resize/reflow and deep-scroll integration checks passed. An independent raw-Taffy probe measures a requested 20-pixel gap as 20 pixels under a stretched 300-pixel parent. The macOS linker emits a pre-existing `__eh_frame` warning.

Visual result: mobile cards have separation; detail sections no longer touch. Correct gaps use more vertical space. Desktop Terminal actions extend beyond Store's explicitly fixed 180-pixel card height; this limitation remains visible because Store code and fixed-height semantics are unchanged. The artwork overflow is also Store-owned and remains outside scope.

Final capture directory: `/Users/xaver/.codex/visualizations/2026/09/15/01a0a50b-c5e5-7e63-bc71-bf4e2b8ac8cd/framework-layout-changes/01-column-gaps-final/`.

Baseline directory: `/Users/xaver/.codex/visualizations/2026/09/15/01a0a50b-c5e5-7e63-bc71-bf4e2b8ac8cd/framework-layout-changes/00-baseline/`.

Captures use a clean detached source snapshot because unrelated tasks changed the shared checkout during verification. Store hashes match the initial manifest; captured framework hashes match the task commits. Tests/captures do not include unrelated unfinished changes.

## Change 2: CardGrid participates in host layout

Commit: `b6ba757ef`.

CardGrid now places children during host layout, and Card has a matching host wrapper node. Buttons receive standard geometry, identities, focus, and input before painting. A shared bounded layout solver supports narrower-than-minimum allocations without rebuilding widget identities. Explicit fixed heights remain fixed; raw custom Taffy hosts use the new helper for that narrow fallback.

Verification: final Card 12/12 and App grid 10/10 tests pass. The broader library run passed 2,966 tests with two ignored before final compatibility cleanup; affected tests were rerun afterward. Independent review approved the change, with minor follow-up coverage for solver rejection/non-settlement.

The unchanged Store probe shows host registration increasing from 7 to 19 focusable widgets and real App focus traversal from 6 to 18. Previously 12 controls had zero rectangles before painting; now none do. Painting previously changed 90 widget rectangles; now it changes none. These counts describe the fixture and do not assert every app in the repository is fixed.

All five Store PNGs are byte-identical to Change 1. This is a lifecycle and keyboard-access improvement, not a visual redesign. Capture directory: `/Users/xaver/.codex/visualizations/2026/09/15/01a0a50b-c5e5-7e63-bc71-bf4e2b8ac8cd/framework-layout-changes/02-card-grid-host/`.

A separate VirtualList post-build synchronization bug was reproduced and assigned to Change 3: retained rows lose node IDs before applying geometry. Ordinary dynamic Card-body geometry is independently tested and passes.

## Change 3: viewport-dependent composition

Commits: `a475555f4`, `367aecb64`. The shared App and headless lifecycle now settles initial and changed logical viewports before painting. `FrameHarness::resize` exposes the same behavior to previews. Retained VirtualList rows keep their layout nodes, and TextInput state includes its selection anchor while remaining compatible with old saved state.

The unchanged Store was resized through 1200 → 920 → 600 → 390 → 1200 using the public harness, without a width hint or an application-specific compose call. At all four destination widths, full inspection JSON and PNGs were byte-identical to fresh instances. At 920 pixels the grid fits in 888 pixels, rather than retaining the former overflowing 1120-pixel width. All five ordinary fresh Store captures remain unchanged. Captured source hashes match nine framework files from the task commit and all 26 Store files from the original baseline.

Capture directories: `framework-layout-changes/03-resize-probe-final/` and `framework-layout-changes/03-resize-lifecycle-final/` under the visualization root above.

The initial implementation passed 194 relevant tests and Clippy. Independent review then identified four required lifecycle corrections: failed-resize recovery to the previous size, modal focus/selection restoration, disabled duplicate focus keys, and mounting native views created during recomposition. All four were reproduced, fixed, and independently re-reviewed without remaining findings. The final fix passes 75 affected tests and Clippy. Native views attach only after successful settlement; focus restoration respects modal boundaries and never copies saved selection to a fallback control. Anonymous replaced controls require stable semantic keys to restore focus.

## Change 4: coherent themed and manual text sizes

Commit: `4e2bf5dc0`.

Role-based Text and `with_scaled_size` now reach their resolved logical font size even when theme and renderer scales differ. Existing default Text, `with_size`, and `with_style` remain renderer-relative. The additive `with_fixed_size` fixes logical font size; letter spacing retains its established renderer-relative contract. Cached glyph scenes invalidate when renderer scale changes, and plugin engines use the same conversion for measuring and drawing.

Verification: 30 Text tests and one custom-canvas regression pass; formatting checks and independent review are clean. An independent probe emits 28px glyphs for themed 14px text at 200%, with either renderer scale 1 or 2. Measurement agrees. Existing base text still emits 14/28px respectively. Captures verify 11 committed framework hashes and unchanged Store source. Four normal-size Store PNGs are identical to Change 3; the existing large-text shot now contains genuinely enlarged themed Text.

Captures: `framework-layout-changes/04-text-scaling/` and `framework-layout-changes/04-text-probe/` under the visualization root above. The existing Store screenshot path still leaves non-Text controls/custom drawing at renderer scale 1. The shared `FrameHarness::new_with_theme` configures both scales correctly and was reused without introducing a separate pipeline.

The fully configured 200% Store capture exposes another framework defect: `Button::fit_content` estimates width from character count without actual font/icon metrics, and button text is centered using an unscaled height. Trust/Remotes/Refresh labels overlap their bounds. This is assigned as a measured-action prerequisite for Change 5, while explicit fixed dimensions remain manual. Store artwork and fixed-card limitations are still outside the Text fix.

## Change 5: content-aware cards and measured actions

Commits: `d45978245`, `12037105e`. Implementation, captures, and independent scoped re-review are complete.

`Card::composed` provides retained header, body, and footer regions. `CardGrid::content_sized` measures each row from its actual content; the body receives spare height so footer bottoms align. `Actions` orders secondary actions before the primary, aligns trailing edges, and wraps measured standard Buttons. Automatic fixture construction requires no caller pixel dimensions or viewport branches. Existing fixed CardGrid heights remain exact; Store still chooses 180px cards and retains the disclosed fixed-card overflow.

`Button::fit_content(height)` now measures real text/icon width and centers the scaled line box. `Button::content_sized` additionally measures height and wraps labels. Explicit size/fill modes remain manual. Independent review found that auto width stretched compact buttons in Column/Grid hosts; the correction pins the measured width through the existing layout-settlement hook without overriding parent alignment. Tests cover retained locale/scale changes, center/end inheritance, explicit fill-width, focus, and click bounds.

Verification: final 39 Button tests pass with one existing ignored visual audit; 64 card/host tests pass; clippy and formatting pass. The frozen snapshot matches all eight task-owned committed files. All 26 Store files remain unchanged. The final card fixture has six captures, including narrow 200% scrolling to the final action. All six images are unchanged by the review correction, and fresh/resized narrow PNG plus full inspection JSON are byte-identical.

Final captures: `framework-layout-changes/05-card-fixture-final/` and `framework-layout-changes/05-store-host-final/` under the visualization root above. The fully configured Store toolbar at 200% now contains its labels; the detail Back button measures 134×40 instead of the intermediate erroneous 736×40.

**Capture/host boundary:** App and FrameHarness use shared layout settlement. Custom manual hosts must use `compute_widget_layout`; direct raw Taffy measurement skips this stage and may stretch fit-content buttons. Store's unchanged legacy `--shots` loop is such a raw host. Final Store evidence therefore uses the public shared FrameHarness, without changing Store or inventing fallback metrics. Historical raw captures in `05-content-cards/` retain this limitation and are not the final actual-host result. Adopting automatic card regions in Store would be a separate application change.

## Change 6: automatic forms and selective overrides

Commit: `838b13acd`. Implementation, captures, and independent review are complete.

`FormPage`, `Form`, `Section`, and `Field` provide themed page background, a single outer gutter, readable maximum width, semantic hierarchy, measured label alignment, supporting/validation copy, scrolling, and shared actions. Wide labels align with the first input; narrow or long/expanded labels stack automatically. Form remains independently embeddable and Section accepts ordinary content. Existing manual fixed control dimensions remain respected.

Supported density, width, spacing, and label-placement overrides are independent and reset with `None`; resolved values retain their actual origin. Read-only layout metadata is available through the widget API. Inspector JSON/UI integration belongs to Change 7.

Draft visual review exposed Checkbox double text scaling and first-control label alignment defects. Both are corrected and covered by measured/drawn geometry tests. Populated TextInput fits its existing height at 200% and is unchanged. Common retained-child plumbing is shared with Card; card layout policy is unchanged.

Verification: 100 affected suite passes (11 form/App, 64 card, 15 Checkbox, five existing form helper, five solver tests), clippy and formatting pass. The final source snapshot matches all 16 task-owned files; capture manifests verify 25 source files in the prior/current union and all 26 unchanged Store files. All five shared-host Store PNGs and full inspection JSON are byte-identical to Change 5. All six card fixture images are unchanged. Automatic form narrow fresh/resized PNG and full JSON match.

Captures: `framework-layout-changes/06-form-fixture/`, `06-store-host/`, and `06-card-regression/`. The form fixture includes automatic/guided/primitive variants at wide, narrow and 200% sizes, plus end scrolling. Automatic construction has zero layout pixel values and zero viewport branches. The source comparison inventories 16 explicit primitive composition concerns; it is not a developer-time or productivity measurement. The primitive example has equivalent content and intent, but retains a Row alignment limitation and is not claimed to be an equally polished completed screen.

Automatic forms require app adoption. Store was not migrated. General manual measure/place, mixed layouts, inspector integration, scaffolding, and the final broad audit remain required.

## Change 7: manual composition and final acceptance

Commits: `b7d136829`, `538161284`. `Manual` and `ManualContext` measure retained standard children and assign exact logical rectangles. Both automatic-inside-manual and manual-inside-automatic fixtures exist. Inspection exposes real effective geometry and policy metadata, and the generated starter uses automatic form composition.

The scoped review found nested negative overflow could disappear from ancestor bounds; the correction and independent re-review are complete. Recovered verification of the isolated framework series passes 3,033 UI tests with two existing ignored tests, 26 Manual tests, two actual external generated-starter tests, and Clippy. Captures verify fresh/resized equality for all three manual arrangements. Store's five PNGs and full inspection JSON remain unchanged from Change 6; 26 Store source files match the original baseline.

The broad final review found two remaining P2 issues: transparent composition containers omit mutable resource-operation forwarding, and changing a manual policy input after a clean painted frame can reuse stale geometry. A separate visual inspection of the actual starter at 200% found NumberInput's default allocation and stepper centering do not accommodate enlarged text. These are being corrected before final acceptance; passing prior tests does not close them.

Final-fix progress: `313ba237a` forwards mutable action admission and passes two new direct/nested regressions. `a7c6da63c` adds an explicit `ManualInvalidation` handle and passes 29 manual tests, including actual painted same-viewport movement, retained focus/selection, both mixed directions, and invalidation surviving probes/failures. Each change received an interim user review ping. Independent review of the combined final fixes and final captures remain pending. NumberInput correction is in progress.

Captures: `framework-layout-changes/07-manual-fixture/`, `07-store-host/`, and `07-starter/` under the visualization root above. These are source-verified isolated captures through `538161284`, excluding the unrelated desktop UX changes in `82afabfb7`.

## Final result

All seven tasks are complete. The final correction `7bebe26b6` gives NumberInput a scaled minimum footprint and measured value width while preserving explicit/manual allocation. Its actual generated-starter 100% and 200% captures now contain the value and centered steppers. The independent final scoped review approves all three fixes with no new findings.

Final corrected isolated verification: **3,041 UI tests passed, two ignored; two actual generated-starter tests passed; Clippy and formatting passed**. Source verification covers 39 framework files and all 26 unchanged Store files. The final Manual54, Form32, Card12 and Store10 PNG/JSON artifacts exactly match their preceding sets. All three manual arrangements have identical fresh/resized narrow PNG and JSON. Starter captures show the intended NumberInput correction.

Final captures: `07-final-manual`, `07-final-form`, `07-final-card`, `07-final-store`, and `07-final-starter` under the visualization root above. All seven behavioral tasks received completion review pings; the final three fix behaviors also received interim pings.

[Acceptance matrix](/Users/xaver/dev/qos-macos-shell/docs/superpowers/audits/framework-layout-verification/final-acceptance-matrix.md) · [Final scoped review](/Users/xaver/dev/qos-macos-shell/docs/superpowers/audits/framework-layout-verification/final-fix-review.md) · [Visual evidence](/Users/xaver/dev/qos-macos-shell/docs/superpowers/audits/2026-09-15-framework-layout-evidence.md) · [Recorded rulings](/Users/xaver/dev/qos-macos-shell/docs/superpowers/audits/framework-layout-verification/rulings.md).

Store retains its authored fixed180px cards and artwork defect. New Card/Form patterns require adoption; no Store migration occurred. Manual input changes require explicit invalidation plus normal host frame scheduling. Probes measure retained topology, and reported bounds are not universal pixel-paint diagnostics. The unchanged raw Store screenshot loop bypasses shared settlement; final captures use FrameHarness. The unrelated qos-dev locale test's case-sensitive-filesystem assumption remains a documented macOS limitation; the generated starter itself passes. No human productivity study or elapsed time ratio is claimed.

The branch and working checkout are retained locally. No push, merge or publication was performed; unrelated changes are preserved.
