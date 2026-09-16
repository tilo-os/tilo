# Task 3 scoped review — Browse and form composition

## Verdicts

- **Specification: CHANGES REQUIRED, with dependency-pending acceptance.** The independent Store, Settings Accounts, and VPN changes satisfy their scoped requirements, but Themes still fails the disclosure's visible/accessibility-state contract and the full specimen is not contained in its declared narrow geometry. Final Store narrow-section containment and Settings Region ordering remain pending the separately assigned Task 5 Card/ContentCard/CardGrid automatic minimum-height fix.
- **Code quality: CHANGES REQUIRED.** Store's typed bindings and width decisions, Settings' composition cleanup, Themes' hidden-child routing/focus return, and VPN's shared geometry table are sound. The two Themes issues below should be fixed before this task is accepted.

## Important findings

### 1. Themes' narrow "Full preview" can draw beyond its declared scroll content

`build_panes_layout` gives the narrow preview a fixed 560px height and records exactly that layout height in `scroll_content_h` (`apps/themes/src/app.rs:894-918`, `apps/themes/src/app.rs:1144-1161`). `PreviewPane`, however, draws every specimen section in sequence and forces the final charts to at least 60px high even when the remaining card height is smaller (`apps/themes/src/preview.rs:214-228`, `apps/themes/src/preview.rs:856-878`).

The fresh `themes/live-narrow.png` and `themes/preview-narrow.png` captures show the result: the chart cards extend about 19–31px below the specimen surface. Because that overflow is absent from `scroll_content_h`, the tail cannot be made fully reachable at the first-class short-height target by scrolling to the computed maximum. This also breaks the containment expected of a view named "Full preview."

**Action:** make the narrow preview's declared/intrinsic height cover the complete specimen (or make `PreviewPane` compute its section height), and include that height in the scroll extent. Add a short-height narrow regression asserting the final chart bounds are inside both the preview/card bounds and the reachable scroll content.

### 2. The new Themes view switch exposes no selected state

The switch is two ordinary `Button`s (`apps/themes/src/app.rs:193-204`). Neither button is updated from `narrow_preview`, so the latest gallery-closed Customize, live-preview, and custom-preview captures render both controls with the same idle styling. A standard Button exposes role/name/focus/disabled only (`ui/src/widget/button.rs:1299-1306`), so assistive technology also cannot determine which view is active. This misses the brief's requirement that the disclosure expose an accessible name/state and makes the two-tab alternative visually ambiguous.

**Action:** use the existing selected-state control (for example, a two-segment `ToggleGroup`) or explicitly synchronize a selected/pressed state and visible treatment from `narrow_preview`. Cover initial editable/live states, pointer and keyboard switching, and the accessibility selected/checked state.

## App-by-app assessment

### Store — independent work accepted; final acceptance pending Task 5

- Fallback artwork now uses an inward inset (`apps/store/src/widgets/artwork.rs:162-168`); the mark coordinates leave room for stroke extent.
- Catalogue width decisions share `CATALOGUE_CONTENT_MAX_WIDTH`, and the latest 800×480 capture has one source summary with the first result beginning around y318, materially above y445 (`apps/store/src/app.rs:101`, `apps/store/src/app.rs:1289-1303`, `apps/store/src/app.rs:2723-2741`).
- Source/category labels are separated from their values, controls can grow, and the 200% stacked capture preserves both selected values (`apps/store/src/app.rs:2397-2463`).
- Cards retain typed Details/primary bindings and operation locks through the production composition (`apps/store/src/widgets/card.rs:80-116`, `apps/store/src/app.rs:2572-2591`). The focused regression covers pointer Details, semantic primary activation, both widths, and busy-disabled behavior (`apps/store/src/app.rs:4459-4533`).
- Normal 1200px visual evidence shows aligned action baselines and contained Terminal/Files/VLC footers. The one-column full-catalogue failure is the known shared minimum-height defect, demonstrated by `apps/store/src/app.rs:4578-4604` and recorded at `task-3-report.md:27-32`; acceptance of that item is pending Task 5.

### Settings — Accounts accepted; Region acceptance pending Task 5

- The redundant outer spacers were replaced by a compact heading/body composition (`apps/settings/src/settings_app.rs:297-318`), and the duplicate Accounts card title was removed while keeping status text and Add account (`apps/settings/src/panes/accounts.rs:478-500`). Latest wide and narrow Accounts captures show the first Email action in the initial viewport.
- The latest underscore-named Region captures are route-correct and move the first field upward. They also visibly reproduce the assigned field/Preview collision. The focused regression records `First day of week` ending at y450 while Preview begins at y436 (`task-3-report.md:27-32`). Region ordering therefore remains pending Task 5 rather than being accepted here.

### Themes — routing/state preservation accepted; disclosure presentation rejected

- Hidden view nodes are omitted from layout, drawing, event routing, and accessibility children (`apps/themes/src/app.rs:894-948`, `apps/themes/src/app.rs:1632-1648`, `apps/themes/src/app.rs:1832-1859`, `apps/themes/src/app.rs:1870-1921`).
- Per-view scroll is preserved, hidden-pane focus is cleared, and Escape returns to Customize while focusing the Full preview trigger (`apps/themes/src/app.rs:841-867`, `apps/themes/src/app.rs:1769-1786`). The focused test also preserves the edit and document heads across view/breakpoint changes (`apps/themes/src/app.rs:2072-2149`).
- Gallery-closed visual evidence confirms editable controls start in the first narrow viewport and live presets open on the specimen. Findings 1–2 prevent acceptance of the full-preview disclosure.

### VPN — accepted

- Compact header, notice, body, controls, rows, and action rectangles all come from one `LayoutRects` computation (`apps/vpn/src/app.rs:1242-1399`). Drawing consumes that table (`apps/vpn/src/app.rs:1580-1713`, `apps/vpn/src/app.rs:3328-3365`), pointer handling recomputes the same table (`apps/vpn/src/app.rs:3912-3971`), and accessibility uses the same bounds (`apps/vpn/src/app.rs:3688-3709`, `apps/vpn/src/app.rs:3781-3798`).
- The fresh 480px error capture shows title/control, Retry/Dismiss, and first connection row as disjoint. Semantic Retry/Dismiss route through the shared recovery functions, while Escape dismisses the error (`apps/vpn/src/app.rs:3819-3864`, `apps/vpn/src/app.rs:4047-4053`). The compact geometry regression checks notice/action/row containment and pointer routing (`apps/vpn/src/app.rs:4103-4134`).
- The unchanged macOS failure for the Linux-only `/sys/class/net/lo` test is outside Task 3 and remains for controller Linux validation.

## Verification basis

Reviewed the authoritative `review-4f0cc6b27-5ca1d1d83.md` package, the brief/controller rulings/report, and the latest named visual artifacts under `task3-resume-20260916`; the earlier final Store wide/narrow/large-text artifacts were used only for the requested dimensions. Per the controller instruction, reported suites were not rerun. No production files were changed.
