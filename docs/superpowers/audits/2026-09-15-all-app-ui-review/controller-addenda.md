# Controller review addenda

## Store artwork bounds — P2, open

Individually inspected `captures/store/wide.png` and `narrow.png`: fallback Files and VLC artwork protrude beyond their tile; Files touches its title. `apps/store/src/widgets/artwork.rs:167` uses `self.rect.inset(inset)` with a positive22% amount. The checked local kurbo implementation expands the rect for positive Insets. Correct to inward bounds (negative amount) and verify all fallback marks remain within the allocated artwork tile, allowing their intentional stroke extent. Include this in the Store composition task; do not compensate by increasing the title gap.

## Scratchpad ownership

Controller inspected its baseline wide screenshot and `apps/scratchpad/src/lib.rs`. The original task Build macOS component demo app is completed/notLoaded. Preserve its untracked feature implementation; the new user request authorizes a minimal layout integration. Original lib.rs and SHA256 are backed up under `.superpowers/app-ui-2026-09-15/scratchpad-original/`. Commit shared framework work separately, retaining an explicit patch/evidence for the untracked app integration.

## Corrected capture findings — ef5f08db5

Controller individually inspected Camera, Sheets, Maps, Files and Music paired-test200% images from `.superpowers/sdd/2026-09-15-app-capture-fidelity/captures/`. Fresh binary/image hashes and test results are in that task report. These are newly valid evidence, not newly introduced composition defects.

- **Camera P1:** top photo option labels and bottom mode/camera labels exceed their fixed-height controls at200%. Main preview remains useful. Source anchors: `apps/camera/src/app.rs::draw_mode_switcher` and toolbar layout.
- **Sheets P1:** toolbar, formula bar, row numbers and cells are heavily clipped/overlapping at200%. `apps/sheets/src/grid.rs` default26px rows and theme-sized draw text require one scale application and measured default row/header geometry. Explicit user row sizes must retain their manual contract.
- **Maps P1:** directions distance/duration, route steps and navigation card subtitle overlap at200%. Measure related rows and reserve each text band; preserve map workspace and route access.
- **Files P1:** sidebar labels, table headings and rows collide at200%; text also appears double-scaled. Normalize resolved typography and update shared paint/hit/scroll metrics together. Additional compact header whitespace remains open.
- **Music:** confirms existing marked200% route-loss finding: alternate list is readable but navigation/search/queue routes remain absent. This is not closed by capture correctness.

## Post-card-fix Region wide composition

Controller individually inspected task5-after-20260916/captures/settings/panel-region_formats.png (1200×760) and narrow counterpart. Shared minimum propagation fixes overlap, but wide Region leaves labels at x312 and control start around x932: a large empty label/control gap weakens grouping. Marked settings-region-wide-balance separately; shared geometry correctness is not visual-balance closure. Remaining App Text/Geometry Task22 owns a bounded Region composition improvement through existing measured Form/Field or readable-width container, preserving all values/actions/manual overrides and narrow behavior. Do not alter all Settings panes globally.
