# Spreadsheet showcase

The example hosts the production spreadsheet offscreen at **1440 × 810** with
`Theme::dark().with_ui_scale(1.5)`. Only this capture seeds the sample budget;
a normal application launch still opens an empty workbook.

## Reproduce

Build from the repository root, then run the fetched executable locally. The
video encoder must be available where the executable runs, not just on the build
machine. It requires `ffmpeg` with `libx264`.

```sh
cargo build --locked -p sheets --example showcase
# Use the complete, verified product catalog from your main checkout.
ln -s "$TILO_CHECKOUT/crates/sdk/ui-core/assets/fonts" target/debug/examples/fonts
./target/debug/examples/showcase "$TMPDIR/sheets-captures" demo
```

The font link must point to the complete catalog, including the font files,
manifest and provenance. If `target/debug/examples/fonts` already exists, verify
its target rather than nesting another link inside it.

Outputs:

- `sheets-showcase.png`: dark, 150% UI scale, formatted budget, two live charts,
  the selected E2 formula, and Ada's F3 cell selection.
- `sheets-demo.mp4`: H.264, 1440 × 810, 60 fps, 210 frames / 3.5 seconds. A
  minimum-jerk curved pointer movement selects the formula bar, then updates
  January groceries from $420 to $680 with real pointer/keyboard input. The
  grid, dependent formulas and charts refresh through the normal application
  path. The capture asserts the stored numeric value, rather than accepting a
  video of an unsuccessful gesture.
- `sheets-light.png`: the same workbook after the interaction, in the light theme.

## Visual audit

The original 150% render showed truncated File, Format, AutoSum and Formulas
labels even in a wide window: the toolbar's renderer-free cache watched text
scale, but full UI scaling also changes spacing and type. Measure the toolbar
against its actual painter before drawing it.

The annotation dock originally disappeared at this size, and its chart height
budgeted only one action row while drawing two. Cards now account for both rows
when narrow, combine every command into one row when wide, and preserve at least
four sheet rows above the dock. The shorter landscape plot and bottom-edge fade
keep more of the sheet readable without an abrupt slice through the final row.

Cell typography, row/column headers, currency/number formatting and sheet tabs
already follow measured theme metrics; retain those paths. Long formula edits
now pan horizontally with the caret, and their click mapping pans with them.
Collaborator selections resolve durable row/column anchors against the local
layout, use the shared themed outline/name chip, and preserve sheet-tab presence
when a peer changes sheets or selects an unauthored cell.

Regression tests exercise the production painter at full 150% UI scale, command
label widths, every chart action target, long-formula caret/hit mapping, and cell
presence across sheet renaming and row insertion.
