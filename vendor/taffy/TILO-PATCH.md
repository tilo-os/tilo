# Tilo: layout on device pixels, flex items measured as laid out

Source: crates.io `taffy` 0.10.1, carried as it ships apart from the two changes below.

- Registry checksum: `aea22054047c16c3f34d3ac473a2170be1424b1115b2a3adcf28cfb067c88859`
- Repository: https://github.com/DioxusLabs/taffy
- Upstream revision: `639f0acdd20f914f847abffbeaa903d66cae4882` (the archive's `.cargo_vcs_info.json`)
- License: MIT (the manifest's `license`; the published archive carries no license file).
- Only the library is vendored: the manifest drops the example targets, and the
  archive's `examples/` and `Cargo.lock` are omitted.

Taffy rounds a solved tree to whole units. Tilo lays out in logical pixels and
draws them at the display's scale, so whole logical pixels are device pixels
only at whole scales: at 1.5 every odd edge fell between two device pixels and
was drawn blurred across both. Layout's rule is "logical units with edges
snapped to device pixels" (docs/designs/app-language/layout.md §5).

- `src/compute/mod.rs`: `round_layout_to_grid(tree, node, steps_per_unit)`
  rounds as `round_layout` does (cumulative positions, sizes as differences of
  rounded edges) to the nearest `1 / steps_per_unit`; `round_layout` is it at 1,
  unchanged.
- `src/tree/taffy_tree.rs`: `TaffyTree::set_rounding_scale` /
  `rounding_scale`; `compute_layout_with_measure` rounds on that grid.
- `src/lib.rs`: exports `round_layout_to_grid`.

The toolkit sets the grid from the display's scale where it solves a tree
(`ui-core` `widget::layout::solve_layout` and `widget::solve_layout`).

Remove this part once upstream rounds to a configurable grid.

## Flex items are measured at the cross size they are laid out at

`determine_flex_base_size` (and the intrinsic main size of a flex container)
measured an item with a definite cross size (`width: 100%`) at that size as it
stands, while `determine_hypothetical_cross_size` lays it out at that size
clamped by its `min-` and `max-` cross sizes. A page's readable column
(`width: 100%; max-width: 40rem` in a column container) was measured at the
window's width and laid out at 40 rem: its text wrapped into fewer lines when
measured than when laid out, and a page under a hugging parent was sized lines
short of its content (13 px past it in Mail's conversation at text 1.5 on an
ultrawide window).

- `src/compute/flexbox.rs`: both measurements clamp the item's known cross
  size by its min and max cross sizes first, as the layout does.

Remove this part once upstream clamps the known cross size there.
