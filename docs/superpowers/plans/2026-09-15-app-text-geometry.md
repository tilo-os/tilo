# App Text and Geometry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close confirmed large-text layout and route failures while preserving pleasant normal-size density and manual authoring.

**Architecture:** Consume the shared resolved-style boundary and couple measured app geometry to paint, hit testing, scrolling and accessibility. Authored slide/document coordinates remain explicit; ordinary app text scales once. No parallel production workers.

**Tech Stack:** Rust, Tilo UI, Vello, Taffy, app-local retained and immediate widgets.

**Spec:** `docs/superpowers/specs/2026-09-15-app-ui-balance.md`

## Global Constraints

- Reuse the repository Cargo target cache; do not rebuild all dependencies into a new target directory without a concrete build-isolation need. Isolate runtime captures with task-specific HOME and XDG_CONFIG_HOME/XDG_DATA_HOME/XDG_CACHE_HOME/XDG_STATE_HOME, separately from build caches.

- Shared `TextRenderer::renderer_input_style(&TextStyle)` normalizes final logical styles for existing renderer-relative measure/draw calls. Theme typography tokens are already resolved; literal base sizes remain relative. Apply normalization to **both** measurement and paint. Do not scatter reciprocal factors, disable renderer scale, or multiply gaps/entire app rectangles by text scale.
- Keep normal100% minima/density, authored document geometry, explicit row/column/pane sizes and existing view state. Automatic heights become `max(existing normal minimum, measured occupied height + padding)`; stacking occurs only when measured groups cannot fit their actual remaining width.
- Cache one geometry result per affected surface. Drawing, scroll extent, pointer hit, keyboard ensure-visible and accessibility consume it. Recompute on width/content/font/scale changes without resetting selected ID, draft, playback or user resize state.
- Each task starts with focused failing geometry/interaction regressions, implements the narrow change, then runs focused crate tests. Read package names from Cargo.toml. No heavy workspace suite during implementation planning. Record commands/results and relevant screenshot provenance.
- Acceptance captures:100%1200×800 and480×900;2×800×900; affected short-height800×480. Review actual pixels, exercise described pointer/keyboard routes, then close findings individually. Merely adding a helper or showing a screenshot does not establish event correctness.


- Preserve unrelated working changes, especially existing dirty tests and untracked Scratchpad source. No dependency upgrades, pushes or merges.
- Focused RED/GREEN behavior tests, scoped package checks, fresh actual screenshots and independent review per task. CARGO_BUILD_JOBS=4; CARGO_PROFILE_DEV_DEBUG=0; CARGO_PROFILE_TEST_DEBUG=0.
- No subagents from implementers. Commit only named/scoped files; do not blanket-stage app directories or format the workspace.

### Task 1: Sheets automatic metrics and chrome


**Confirmed:** corrected-review-2 Sheets P1: row slices, indices, formula, menus and tabs overlap. No baseline1× density defect.

**Files/anchors:** `apps/sheets/src/grid.rs`: constants26–31, manual maps208–213, `row_h`707, `row_top`721, body/header origins732/810/813, `visible_rows`898, `has_layout`985, drag overrides1507–1511, autofit1896, cells2037, headers2322/2386. `apps/sheets/src/app.rs`: FORMULA_H51, formula/menu/tab paint3611/3787.

- [ ] Add per-view resolved grid metrics (default row height, column-header height, row-header width, text style), computed from actual default cell/header font measurement after shared normalization. Replace *automatic* DEFAULT_ROW_H fallbacks, including the out-of-range `row_top` extrapolation, with the effective default. Replace all header-origin uses with those metrics, including scrollbars and hit calculations.
- [ ] Preserve `row_heights` and manual `col_widths` overrides byte-for-byte. `row_h(r)` remains `manual.get(r).unwrap_or(effective_default)`. Keep automatic column autofit separate from manual widths (existing comments already establish this). Rebuild prefix offsets/cached visible rows when default metrics change; retain active cell and scroll anchor rather than numeric pixel offset alone where possible.
- [ ] Explicit short rows may still clip enlarged cell content: retain their chosen physical height, keep cell editor/formula editing readable, and retain intentional row resize/autofit route. Do not silently rewrite document sizes, enlarge manual columns, or auto-persist accessibility-derived geometry. If an explicit zoom is added later, it is a view transform applied coherently; no new zoom system is needed for this task.
- [ ] Measure menu labels, formula line and sheet tabs with the styles actually painted. Automatic band heights increase only as needed; wrap/overflow menu commands through existing menu routes. Formula editor and point-mode address/reference highlights share the same updated band rect.

**Regression cases:** scale1 retains26px default row/28px header when metrics fit; scale2 automatic row contains text; user26px row and200px column unchanged across1→2→1; selected far row retains cell identity; header click, drag resize, cell double-click editor, fill handle and formula point mode target same cell before/after reflow; both scroll thumbs map to new content extents; long tab/menu labels remain activatable. Existing `visible_scrollbar_thumb_is_pointer_draggable_on_both_axes`, `autofit_grows_column_to_fit_label_past_occupied_neighbour`, and `header_highlight_tracks_selected_columns_and_rows` are focused regression neighbors.

**Boundary:** one Sheets pass, no shared table abstraction.


### Task 2: Slides document coordinates and chrome


**Confirmed:** corrected-review-2 P1: canvas title/bullets exceed unchanged authored bounds; toolbar groups collide.

**Files/anchors:** `apps/slides/src/render.rs::draw_text_element`260 (authored `e.font_size * s`280, line advance284, alignment measure and draw331), other canvas text draw217/243; `apps/slides/src/app.rs` title822/827 and toolbar composition3593; existing selection/edit/inspector geometry consumers.

- [ ] Treat canvas font pixels (`e.font_size * canvas_scale`) as **resolved document/view pixels**. Build the style with authored weight/italic and normalize it through the shared helper before measurement/draw. Retain the same resolved line-height for wrapped-line advances. Apply the contract consistently to slide thumbnails, main canvas and presentation rendering; do not normalize UI base fonts accidentally.
- [ ] Authored slide/object bounds, selection handles, caret and text hit testing must use the same canvas transform/inverse and line wrapping. Accessibility text size alone must not change element dimensions, object position, authored font size or exported document geometry. Inspect renderer calls beyond260 so text placeholders/labels intentionally belonging to the document share the policy.
- [ ] Independently measure toolbar title plus Add Slide/More/theme/Present. Reserve primary Present and overflow first, elide title into the remainder, and route omitted commands through existing overflow. Grow automatic toolbar height if its line box needs it; preserve useful slide letterboxing and manual canvas zoom.

**Regressions:** same slide at renderer1 versus2 has equal document glyph bounds and wrapping, unchanged element/document state and export dimensions; UI text visibly scales once. Long title and final bullet stay inside authored bounds at canvas zoom50/100%; select/drag/resize/edit same object and place caret at wrapped line; main/thumbnail/Present parity. At480/800 long toolbar title must not occlude Present or More; activate each hidden command with pointer and keyboard.

**Boundary:** document rendering contract and toolbar consume the same pass, independently testable from other apps.


### Task 3: Video Editor measured controls


**Confirmed:** corrected-review-2 P1: inspector metadata/labels/sliders/transition sections collide, transport timecodes crowd, ruler/lane controls overflow. Normal-scale layout is useful.

**Files/anchors:** `apps/video_editor/src/app.rs`: constants58–69 (TIMELINE_H304,RULER_H30,TRANSPORT_H58), section metrics139/header232, `lane_rects`2355, `clip_rect`2371, `transport_buttons`2463, `inspector_metrics`2667, `inspector_layout`5868, `inspector_max_scroll`6738, `inspector_row_rect`11072, `draw_inspector`15394/body15497, `draw_section_header`16626, `draw_timeline`16926.

- [ ] Add measured vertical metrics to existing InspectorLayout; title/track metadata, property label/value/slider and transition groups advance from measured bottoms. At insufficient width place labels/values above slider track, not on it. Existing inspector scroll support must receive the true content extent; retain section folding, selected clip and numeric draft values.
- [ ] Feed new inspector row rects into slider hit/focus bands, keyboard ensure-visible and `a11y_inspector_rows`9556/`a11y_inspector`10031. Hidden/folded controls remain absent from focus and stale hit rects.
- [ ] Derive automatic transport/ruler/lane heights from actual text metrics. Measure current/total duration as a group; wrap or separate it from playback buttons when needed. Ruler tick-label spacing must use measured widest label to avoid printing overlapping ticks.
- [ ] Keep seconds↔x (`time_to_x`2204 / `x_to_time`2207), timeline zoom, clip times, lane assignment, selected playhead and media aspect ratio unchanged. Updated lane rectangles drive clip dragging, trims, drop lane and a11y timeline. Ensure automatic timeline height does not starve preview on800×480: allow the existing panel collapse/scroll routes, no global enlargement of304px by scale.

**Regressions:** selected clip all inspector sections reachable at2×; dragging slider adjusts intended field, keyboard arrows match; fold/unfold preserves value and scroll; transport duration remains distinct; ruler ticks non-overlapping; x→time round-trip unchanged; dragging/trim on second/last lane hits correct clip after reflow; bin, filters/export controls reachable at short height. Extend `folded_sections_hide_controls_but_report_their_state` (case `folding_a_section_hides_its_controls_but_never_its_state`), `reader_reaches_every_inspector_control_it_can_adjust` (case `an_inspector_slider_can_be_adjusted_without_a_mouse`), `timeline_geometry_maps_time_lanes_and_canvas` (case `lane_rects_cover_all_tracks`).


### Task 4: Music route parity


**Confirmed:** corrected-review-2 and baseline P1 independently show missing navigation/search content; not a typography collision.

**Files/anchors:** `apps/music/src/app/render.rs`: large-text branch2090, `render_large_text_frame`2194, search rect zero2232, route match2244 onward, normal nav2540, normal search layout2736–2767, search results6378; semantic tree/search handling1238/1344/1395 and input routing1563–1840.

- [ ] Keep the readable large-text row treatment but add visible navigation trigger wired to existing `Action::ToggleSidebar`/View navigation and queue trigger wired to existing action. Reuse normal drawer/sidebar behavior with measured rows; all normal destinations remain reachable.
- [ ] In Search render a real search input, type filters and actual query results. Remove the blanket zero external layout for active Search; keep its focus/edit/event/accessibility geometry synchronized. Do not map Home/Search/Artist indiscriminately to all tracks via fallback.
- [ ] Implement route-specific content for Home, albums/artists, playlists, liked/downloaded and selected album/playlist using existing data/renderer helpers where practical. Preserve query, nav history, scroll per destination, current playback and queue while switching layouts at1.35. A single simplified track list is acceptable only on actual track-list destinations.

**Regressions:** at2× navigate Home→Albums→album→Back, Playlist→track, Search enter query→change filter→play result, open/modify queue; test pointer, Tab/Enter and existing semantic actions. Verify Search result IDs match library search, not all tracks; search input has nonzero visible rect; crossing1.35 does not erase query/history/current track. Local sample playback fixture suffices; report live audio limits.


### Task 5: Camera control containment

**Camera:** corrected-review-1 P2; `apps/camera/src/app.rs` mode switch2529, `ctrl_pill`3275, bottom controls3429. Normalize only resolved theme styles; center text using actual measured height, grow automatic pill/mode heights enough to contain text, propagate those rectangles to pointer and semantic targets. Keep preview margins and shutter position/aspect policy useful at1×. Tests: Photo/Video/Scan,12MP/HDR/Timer on/off, gallery and no-device recovery at1/2; no label below background, modes distinct, activation targets agree with paint.


### Task 6: OSK measured key and suggestion rows

**OSK:** baseline P1; `apps/osk/src/widget.rs` key1078/prediction1146/intrinsic rows1293; toolbar rects `apps/osk/src/toolbar.rs:89`. Measure actual scaled glyph/word height and center with `(rect.height-measured_h)/2`; derive automatic key/suggestion row minima without altering key identity/order. Use same rows for hit map and overlay requested height. Long predictions require bounded label fitting or wrapping without reducing font; do not enlarge only painted keys over stale hit boxes. Tests alphabetic/numeric/shift/suggestions at480 and2×; each glyph/prediction inside owning rect, each click emits same key/suggestion, keyboard overlay remains anchored and doesn't claim transparent remainder as wasted UI. Controller owns Linux capture/live input-method validation and initial VM-state restoration.


### Task 7: Contacts measured information layout


`apps/contacts/src/app.rs`: ROW_H87, list2785, detail composition2944/header3028. Cache measured list row title/subtitle heights and detail identity/group/header bottom; separate phone/email labels from values, preserving already-working wrapped address/notes. Measure Sort/Group selected labels after normalization. Route selection, virtual scroll, pointer row, accessibility and keyboard scroll-to-ID through cached rows. Switch to existing push layout based on measured remaining detail width, retaining user divider ratio when returning wide. Tests long contact name/role/chips and address, list scrolling/selection, detail→Back, edit/save/cancel and field actions at2×. No global fixed62px enlargement for1×.


### Task 8: Disks measured information layout


`apps/disks/src/app.rs`: drive rows987/header1109/overview1516/legend1647/SMART1941. Measure drive name+device+capacity, advance usage bar below; header actions/legend/SMART label-value pairs wrap or stack within pane. Feed row rectangles to selection/scroll/a11y. Keep explicit device/action behavior and confirmation paths untouched. Tests select last drive after scroll, SMART/partitions, error/status and confirmation. **Capture closed drawer at480/800×2 before changing forced56/44 overview columns**; current dimmed detail proves local text collisions, not entire closed-drawer stacking failure.


### Task 9: File Explorer measured information layout

**Controller update 2026-09-16:** Verify current concurrent FileView/PlacesSidebar/NavShell repairs first; source hashes match the accepted external UX-bench evidence at inspection. Preserve their dirty ownership and add only residual repairs after a fresh check. Evidence and limits: `.superpowers/app-ui-2026-09-15/external-ux-evidence.md`. Existing populated grid/long-name/manual-column/rename/post-scroll interaction acceptance below still applies.


`apps/file-explorer/src/places_sidebar.rs`: ROW_H44, token paint383; `apps/file-explorer/src/files_app.rs` NavShell7995. Normalize sidebar/header/List/Grid labels first; remeasure sidebar row and table header minima, preserve user column widths. Use current list/grid row geometry owners for text, edit field, hit targets, keyboard ensure-visible and scroll extents. Constrain/elide header labels within explicit widths, retaining accessible names; do not resize user columns silently. Tests populated list+grid, long file/place names, Modified header, rename editor, selection/keyboard navigation and drag/drop target after scroll. Empty folder is insufficient acceptance evidence. Narrow redundant chip band is a separate normal-size opportunity; use Mail's opt-in NavShell dependency only if its shared ownership option fits Files without a second chip.


### Task 10: Maps measured information layout


`apps/maps/src/app.rs`: nav surface324, `draw_directions`3636, route-step bands3857, time picker3957. Measure duration/arrival as separate summary lines; route step instruction and distance get distinct measured bands. Cache step positions/content extent and use for scroll, selection and route command hits; mode/Leave controls grow to actual line height. Preserve map viewport/manual pan/zoom and route state. Tests long street instruction, duration/arrival, all modes including transit, time picker, search/result→directions→End, drawer collapse and map recovery at2×. No permanent expansion of map margins at1×.


### Task 11: Monitor measured information layout


`apps/monitor/src/app.rs`: ROW_H171, header1369/summary1681/table2289/detail2684, `primary_filter_row_width`2783, layout4992/5011. Use normalized token styles, then measured clock/uptime rows, summary label/value/chart allocation, toolbar groups and table row height. Maintain a row-height metric shared by virtual rows, scrollbars, process click, keyboard ensure-visible and accessible bounds. Do not rebuild tree selection from an index on sort/filter; preserve PID identity. Selected detail/status/action groups wrap with true content height. Tests Resources, filter/sort/expand process tree, select last scrolled row, scrollbar drag and stop confirmation,1× density and2× readable charts. Existing narrow push, row scrollbar and summary-strip tests (6536,7157,7217,7330) must retain intended behavior; change assertion only for automatic scale-sensitive height, not to conceal content loss.


### Task 12: Imager measured information layout


`apps/imager/src/app.rs`: `compute_layout`1233, `layout_source`1332 / drive1369 / confirm1393, title1718/drop1774/selected1870/release2057/recents2156. Paint currently exposes fixed-band crowding. Build one measured layout for title/subtitle, selected image title/path/config, release and recent rows; subsequent sections start at previous measured bottom. Preserve Continue footer and bound/scroll the content area rather than painting beneath footer. `Hit` mappings477/494 and accessible enabled states523 must use identical rects. Tests empty/selected Source, long paths, release/recent selection, Drive/Customize/confirmation, progress/error/result; next/back preserves selection and footer remains reachable. Do not run flashing as validation; use provider/recording fixtures and existing confirmations.


### Task 13: Weather forecast and city row metrics


**Confirmed:** baseline review-3 P1; normal forecast grouping useful. `apps/weather/src/core.rs` CHIP_H62/HERO_H168/TILE_H78 at57–70, list extent909, paint/reorder1213–1272, row hit1554, header2090/hero2195, metric tiles2367, day rows2660, reorder3432–3438, content-height accounting3608/3616; app shell `apps/weather/src/app.rs:745`.

- [ ] Normalize token styles, measure city/date/region header, chip identity/temp and hero temperature/condition as related groups. Stack temperature/unit or identity/temp only when widths fail. Derive automatic tile/hero/forecast row heights from measured content; keep normal minima and existing gaps.
- [ ] Share measured chip row geometry with `ListMetrics` reorder, drag insertion slots, hit testing, scroll extent and keyboard selection. Share detail section heights with scroll accounting, hourly clipping and daily value layout. Do not update paint alone while3608 retains fixed heights.
- [ ] Choose NavShell push from actual remaining forecast/overview width needed by scaled content; retain selected city, manual panel choices and forecast scroll. No blanket340px rail enlargement.

**Tests:** long city/date/region, negative and three-digit temperatures, units and long conditions; last saved city clickable/reorderable;2× forecast all hourly/daily values reachable via scroll; add/select/back;1× narrow detail retains existing useful card stack.


### Task 14: Hex editor coherent byte metrics

**Hex:** `apps/hex_editor/src/view.rs::fit_bytes_per_row`519, metrics627, `hit_test`631, apply_layout1881/adaptive bpr1917, `GridMetrics`2452–2508. Replace fixed char/hex-column/row pitches with measured mono base font glyph advance and line height. Same GridMetrics computes offset/hex/ASCII positions, selection/caret, visible rows, scroll extent and byte hit. Fit bytes-per-row using these pitches; preserve manually chosen bpr if such an explicit mode exists, otherwise recompute adaptive bpr. Keep selected byte offset stable, recompute its row/column and ensure visible. Header title/pills measured separately. Tests 8/16-byte layouts, offset boundary and ASCII/hex click equivalence, final partial row, search result visibility, keyboard nibble edit, scroll and resize1→2→1; fixture whitespace for73bytes remains intentional.


### Task 15: Logs coherent row and detail metrics

**Logs:** `apps/logs/src/app.rs` ROW_H71, extent385/ensure-visible396, filter bands591/649, rows1744/1758, detail2059, accessible rows2313, `column_x`2386, apply_layout2559, `row_at`3132; `responsive.rs:24`. Measure filter/chip groups and reserve their occupied height; fixed table columns become measured priorities within actual content width, with elided metadata and readable message. Derive automatic row line height shared across all listed index↔y paths. Long detail message wraps at actual pane width, contributes scroll extent, and retains full selectable/copyable content. Choose existing push list/detail earlier when measured content cannot fit; maintain selected entry ID/query/filter state. Tests filter toggles and long source names, last scrolled row click matches keyboard selection,2× detail→Back, multiline/long message horizontal clipping absent and copy exact; preserve1× narrow push composition.


### Task 16: Tube measure-first verification


Baseline review-3 P1 identifies Home/card metadata overlap, but no algorithm defect has been established in app-local Flex. `apps/tube/src/screens.rs:222/238/248/354` composes retained Text/Flex; `apps/tube/src/widgets/flex.rs::build_layout`491 delegates children to Taffy.

- [ ] After renderer-aware capture/layout and shared Text fixes, recapture2× Home and inspect measured child bounds before any code edit. Add a focused production-layout regression asserting heading bottom precedes grid and each card's title/channel/count precedes next row; compare1× useful composition.
- [ ] If failure remains, trace scale/font cache invalidation and child measured height into grid row sizing. Remove stale fixed card metadata height only where found; use existing intrinsic Flex columns with related gaps. Reflow app rail labels and minimum card width based on measured content, retaining selected destination and scroll. No shared Flex rewrite or new layout DSL.
- [ ] Verify long title/channel, multirow grid480/800/1200, Home→detail→Back, search, rail navigation and playback controls. If corrected layout passes, record evidence-based closure without speculative implementation.


### Task 17: Calculator measured modes and expressions

- [ ] Calculator:baseline P1 `apps/calculator/src/app.rs:158` mode segment widths and `graph2d.rs:275` expression inset/actions. After normalized styles, measure segment labels and formula/action reservation; preserve graph canvas, use existing sidebar/overlay when insufficient. Test graph expression editing and keypad at2×/short height.

**Acceptance:** Fresh normal/narrow/200% capture of the named flow; focused geometry and pointer/keyboard regression; report any remaining provider limitations.

### Task 18: Ebook reading reflow

- [ ] Ebook:baseline P1 `app.rs:8982`44px TOC rows,7544 page geometry/8148 line paint. Derive TOC rows and pagination from actual chosen reading font/line height; preserve reading-position anchor across reflow, chapter selection and notes. This is its own document reflow task, not covered by Slides fixed canvas policy. Test long chapter title, last line/page turning and annotation selection.

**Acceptance:** Fresh normal/narrow/200% capture of the named flow; focused geometry and pointer/keyboard regression; report any remaining provider limitations.

### Task 19: Photos metadata containment

- [ ] Photos:baseline P2 `app.rs:2551` fixed status band: measure metadata height or disclose optional metadata without losing filename; preserve image canvas/zoom. Test long filename and library/More actions2×.

**Acceptance:** Fresh normal/narrow/200% capture of the named flow; focused geometry and pointer/keyboard regression; report any remaining provider limitations.

### Task 20: Remote roster and header geometry

- [ ] Remote:baseline P1 `app.rs:4206/4365/4740` title/count/New connection and empty-state clusters: shared normalized style + measured heading/action grouping; test empty and populated roster, form/session/back at2×. Preserve provider state and actions.

**Acceptance:** Fresh normal/narrow/200% capture of the named flow; focused geometry and pointer/keyboard regression; report any remaining provider limitations.

### Task 21: Terminal idle and populated text verification

- [ ] Terminal:corrected-review-2 idle token mark `main_body.rs:3591/3592` needs normalized resolved font; sparse frame shows no new collision. Populated VT output must validate cell font advance, cursor, selection/copy and scroll before claiming2× complete. Do not apply UI font normalization blindly to terminal character-cell rendering.

**Acceptance:** Fresh normal/narrow/200% capture of the named flow; focused geometry and pointer/keyboard regression; report any remaining provider limitations.

### Task 22: Verify and finish scaled Settings, Themes and VPN

**Files:** `apps/settings/src/sidebar.rs`, `settings_app.rs`, existing control helpers; `apps/themes/src/widgets.rs`, `preview.rs`, `app.rs`; `apps/vpn/src/app.rs` only if residual app geometry remains.

**Dependency:** Normal app composition Task3 and all shared measured component fixes. Preserve their new ordinary-size spacing and disclosure behavior.

| App | Additional work | Verification |
|---|---|---|
| 3 Settings | Baseline sidebar210 and settings_app297 scale mismatch; normalize custom token calls then verify actual sidebar/search/account headings/actions. | All category/back/search routes at2×; don't enlarge entire form spacing. |
| 3 Themes | `widgets.rs:33/281`58px gallery rows and `preview.rs:140` sample geometry need measured title/subtitle and specimen bounds. | Preset selection/scroll and customizer changes; Show preview route retains edits; sample scale depicts intended theme honestly. |
| 3 VPN | Shared EmptyState handles core defect; consume normalized measured header/notice geometry, then verify list/import at2×. | Retry/Dismiss/Add/import, no negative/clipped state action. |

- [ ] First recapture these apps at100% and200% after shared fixes. Close any resolved finding with evidence; fix only residual verified defects. Test every new disclosure at200% and ensure original view state survives. Do not recreate the long normal-size preamble.


- [ ] Additional confirmed normal-size finding `settings-region-wide-balance`: after the shared card fix, Region wide labels x312 and controls x932 are visually disconnected by excessive horizontal space. Scope Region pane composition (include `panes/region.rs` if needed): use existing measured Form/Field or a readable bounded form width, preserving selected values, row interaction, narrow layout, and explicit manual control dimensions. Do not globally cap all Settings/editor panes or blindly increase padding. Fresh wide/narrow before/after evidence required. See controller-addenda.md and task5-after-20260916 Region images.

### Task 23: Calendar event form residual geometry

**Dependency:** Shared2 accepted. Normal form width and group hierarchy already work; preserve them.

- [ ] Inspect actual 2× event form in task2-after-20260916/captures/calendar/large-text.png. Metadata chips are shorter than labels; NOTES label crosses separator. Trace resolved text measurement and allocated heights in existing app-local controls/section label.
- [ ] Derive chip/section height from the actual painted style and shared measured APIs where appropriate, use common rectangles for paint/hit/focus. Preserve readable bounded normal form; don't globally enlarge gaps.
- [ ] Test long metadata, ordinary and2× row containment, event editing and keyboard/pointer chip selection; verify editor tail is scroll-reachable in compact/short view without losing date/title actions. Capture normal/narrow/2× and short event form.

## Acceptance/closure record

For each task record: exact changed sources, normal1× preservation evidence,2× screenshots and route, focused test result, manual/semantic interaction result, and remaining platform/provider limits. Distinguish fixed capture fidelity, fixed scale magnitude, fixed layout, and verified interaction. None of those is implied by this design document.
