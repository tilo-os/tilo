# Corrected screenshot review 2

Read-only production review, 2026-09-15. All nine assigned apps reviewed individually in both same-size baseline and corrected large-text images (18 images). No builds, tests, production edits, commits or subagents. Baseline entries in review-1/2/3 were read; this report only supersedes scale-fidelity conclusions and adds findings exposed by corrected evidence.

## Evidence and scope

All nine manifests record source `ef5f08db5aece7eaf199067bc575a07862f594ac` and working diff SHA256 `7f762ea1f3d802de77724201200fa2412477d420d59c0bebe82fe0571b479797`. Both compared variants report successful capture. Source anchors below refer to that commit, inspected using `git show`/`git grep`; concurrent production changes are not represented by these images. Working baseline diff remains part of capture provenance. Comparison is 800×900 at requested text scale 1 versus 2, not different window sizes.

Exact individually viewed files (prefix `.superpowers/app-ui-2026-09-15/capture-corrected/captures/`):

- `sheets/scale-baseline.png`, `sheets/large-text.png`
- `slides/scale-baseline.png`, `slides/large-text.png`
- `typst_editor/scale-baseline.png`, `typst_editor/large-text.png`
- `video-player/scale-baseline.png`, `video-player/large-text.png`
- `video_editor/scale-baseline.png`, `video_editor/large-text.png`
- `word-processor/scale-baseline.png`, `word-processor/large-text.png`
- `music/scale-baseline.png`, `music/large-text.png`
- `terminal/scale-baseline.png`, `terminal/large-text.png`
- `store/scale-baseline.png`, `store/large-text.png`

## sheets — reviewed, P1 layout failure

**Capture fidelity closed:** CLI-requested scaling now visibly affects menu, formula, cell values, indices and tabs. Earlier unchanged-image validation gap is closed; this is not a 200% layout pass.

**P1:** Nearly the entire working surface is unreadable. Menu labels overlap horizontally, formula is vertically cropped, cell text is cut into thin slices by unchanged row heights, row indices overlap, and sheet tabs clip at the bottom. Text appears approximately four times baseline for token-sized labels, consistent with the shared double-scale concern. Source ownership: app geometry `apps/sheets/src/app.rs:51` (34px formula), `:3611`, `:3787` (token text); grid `apps/sheets/src/grid.rs:708` (manual/default row heights), `:2037`, `:2322`, `:2395` (token sizes). Repair shared token/renderer scale contract first, then use measured chrome and minimum automatic row heights at increased accessibility scale. Preserve 100% density and explicit user row/column dimensions; provide deliberate zoom/readable editing behavior instead of blindly rewriting manually sized sheets. Verify cell editor, formula point mode, menus and tab hit regions.

## slides — reviewed, P1 layout/content failure

**Capture fidelity closed:** title, controls and slide text visibly enlarge.

**P1:** Toolbar title runs into Add Slide/overflow/theme controls; Present extends past its allocated width. More seriously, the selected title overflows its selection rectangle and body bullets spill below the unchanged slide canvas, losing the final bullet. This is a fixed-document coordinate contract problem as well as chrome sizing. Source: `apps/slides/src/app.rs:822`/`:827` title text, toolbar composition in `:3593`; `apps/slides/src/render.rs:260`, `:280`, `:284`, `:331` derives document font/line metrics from element size and slide scale while rendering uses the shared text renderer. Keep authored slide dimensions/font sizes and object hit boxes coherent; separate UI accessibility scaling from document rendering (or introduce an explicit coherent canvas zoom). Measure toolbar occupancy and use existing overflow when required. Do not remove useful slide letterboxing. Verify caret/selection, inspector and Present after repair.

## typst_editor — reviewed, P1 toolbar and gutter failure; existing P2 export route remains

**Capture fidelity closed:** source and chrome enlarge, compiled preview appropriately retains document geometry.

**P1:** At 800px the left action group collides with right zoom controls: Save/Open/zoom labels overlap. Source lines wrap, but gutter numbers are excessively large, clipped at the left edge, and overlap adjacent visual lines. Source: `apps/typst_editor/src/app.rs:103` fixed 46px toolbar, `:113` compact threshold760, `:1111` full toolbar packs groups independently. Shared editor gutter ownership: `ui/src/widget/code_editor.rs:397` delegates to `ui/src/widget/text_input.rs:1018`; trace scaled font/gutter metrics together. Use measured effective available width to select compact/overflow; size gutter from actual line-number metrics once and maintain row alignment. Preserve 1x editor density and user split/preview zoom.

**P2 retained:** Export PDF remains absent at800px. `apps/typst_editor/src/app.rs:1128` only adds Export at1120px, while compact overflow begins below760px. Add real overflow access throughout the intermediate range. This was already identified by baseline; corrected screenshot reconfirms it, not a newly introduced issue.

## video-player — reviewed, no new confirmed collision

**Capture fidelity closed:** Library, Demo clip, Open and time labels visibly scale. Compared800px frame retains distinct top controls and bottom transport; media letterboxing is appropriate. No additional spacing defect asserted. App uses shared player sizing (`apps/video-player/src/app.rs:2138`); keep it while checking smaller widths and longer titles. Current image does not establish library, open-file, subtitles, actual playback, keyboard or seek hit alignment at2x.

## video_editor — reviewed, P1 inspector/transport/timeline failure

**Capture fidelity closed:** labels and timecodes now clearly enlarge.

**P1:** Inspector title/track metadata crowd each other; property labels and values paint through slider tracks, transition choices merge, and Label/Curves sections touch. Transport current time loses a readable total-duration relationship; timeline ruler labels/timecode crowd each other and left Mix/track/audio/video controls overflow their short regions. Source: `apps/video_editor/src/app.rs:58`–`:69` fixed toolbar/timeline/ruler/transport/lane heights, `:139` section height, `:232` inspector header, `:15394` inspector, `:15497` inspector body, `:16926` timeline. Measure scaled inspector rows and control groups, stack where needed, allow inspector scrolling, and increase only automatic layout minima at elevated scale. Preserve explicit timeline scale and normal editing density; do not globally enlarge media geometry. Verify short-height preview/transport, bin, filters and export.

## word-processor — reviewed, P1 layout bar collision; P2 toolbar crowding

**Capture fidelity closed:** toolbar, ruler, placeholder and layout bar visibly enlarge.

**P1:** Bottom Margins/Spacing chips, page-layout hint and document counts overlap into one unreadable line. Source: `apps/word-processor/src/widget.rs:3714` layout bar, `:3739` estimates chip width by character count, and subsequent readout/status estimates. Measure actual scaled labels; reserve status first, hide optional hint when it cannot fit, and size hit boxes/height coherently. Preserve document margins/page dimensions and user zoom.

**P2:** Toolbar Styles and More touch and the right zoom controls are crowded; source `apps/word-processor/src/widget.rs:11205` toolbar. Use measured spacing/overflow at effective width. Blank page whitespace is the writing workspace, not a defect. This blank fixture cannot prove rich paragraph wrapping, caret selection or pagination at2x. Prior480px page-fit readability issue is not re-evaluated by these800px images.

## music — reviewed, P1 route parity failure independently confirmed

Text-scaled list is visually readable; this is not a collision report. At2x the normal navigation sidebar and Home cards disappear, replaced by all tracks beneath Home. The search icon remains visible, so the precise issue is loss of usable search content/navigation, not disappearance of the search button.

**P1:** `apps/music/src/app/render.rs:2090` switches wholesale to `render_large_text_frame` at1.35; `:2194` paints its own frame without navigation, explicitly sets the search input external layout to `Rect::ZERO`, and selects track IDs using a match whose fallback includes Search/Home rather than rendering search results. Preserve normal destinations through a compact navigation/overflow control; render a real search input/results and route-specific content within the readable layout. Preserve baseline grid/manual dimensions at1x. Static images plus source confirm missing rendered navigation/input; live keyboard and pointer route tests remain needed.

## terminal — reviewed, capture fidelity closed; shell flow still unverified

CLI-requested2x visibly enlarges idle qOS Terminal. It is approximately4x baseline rather than2x, consistent with token+renderer scaling: `apps/terminal/src/main_body.rs:3591`–`:3592` uses typography lg. **P2:** correct shared scaling contract before using this mark as acceptance evidence. No collision or whitespace issue in the sparse idle surface itself. Prior environment-based scale evidence was already present; this closes the CLI-only handler gap, not a new terminal-shell validation pass. Require populated VT output at2x with cursor, scrolling, selection/copy and character-cell hit alignment; cold-start mark cannot establish those.

## store — reviewed, P2 selected-filter visibility; earlier scale inconsistency closed

**Capture fidelity closed:** search, filters and card actions now visibly enlarge along with card text. The previous shared-scale inconsistency between small controls and large cards is not present in this corrected pair. Large-text single-column cards are readable and actions stay inside the first card.

**P2:** Source and Category selected values disappear behind their fixed-width dropdown affordances (both display only prefix plus dot instead of baseline All), making the current filter state unavailable visually. Source `apps/store/src/app.rs:2408`, `:2433` builds combined prefix/value labels; use measured widths, wrap/reflow or prioritize the selected value when constrained. Keep search/filters/actions reachable. Baseline card action overflow is still visible in the1x control image and remains the prior finding; do not call it fixed from2x card success. No new claim about header whitespace is needed. Verify filter menu selection, search results and install/detail routes at2x.

## Remaining limits / bounded repair order

1. Correct shared single-scale and editor gutter metrics before compensating app geometry, especially Sheets and Terminal.
2. Preserve authored slide/document coordinates; repair Slides canvas scale contract separately from chrome accessibility.
3. Fix measured app rows/toolbars in Sheets, Typst, Video Editor and Word Processor, keeping1x density and manually chosen dimensions intact.
4. Restore Music route parity and Store selected-filter visibility.

All nine apps have an explicit disposition above. Capture-fidelity closure means requested scaling now reaches rendering; it does not imply correct scale magnitude, accessibility or working interactions. No narrow files were required to explain the same-size comparison and none are claimed newly viewed. Existing baseline reviews remain authoritative for unrelated routes. Static screenshots cannot prove live hit testing, keyboard navigation, media playback, exports, populated documents or scrolled content. New truthful captures of those specific flows should follow bounded repairs.
