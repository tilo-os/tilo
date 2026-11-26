# Tilo: layout on device pixels, flex items measured as laid out

Source: crates.io `taffy` 0.10.1, carried as it ships apart from the three changes below.

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

## A measurement answers another constraint only when its content gives it back

`Cache::get` answered a query that knows a dimension with any entry that came to
that same size, whatever space the entry was measured under. Under a min-content
constraint a block of text wraps into its narrowest lines, so a card holding it
comes to the height of that wrapping; a card whose content names a width it will
not go under (`min-width: 400px`) comes to that same width there. The solve then
asks for the card at 400 px wide and is handed the min-content height: the card
stood 393 px tall in the 87 px row of a transcript that centres it, its content
87 px at its top and the rest past both edges of the row.

Answering only the space an entry was measured under fixes that, and costs every
nested grid or flex container a fresh solve of its subtree for each constraint
its parents probe it under: a page of forms solved its tree thousands of times
over. So each entry records, per axis, whether laying the node out at the size it
came to would give it back (`Taint`), and an untainted entry answers as upstream:

- width: a leaf measured with its width open is measured again at the width it
  came to (that answer is cached too); another height there taints it. A
  container that came to a minimum width of its own above zero is tainted, and a
  width taint travels to every node solved from the tainted one.
- height: a container that came to a minimum height of its own above zero, or
  that holds a child whose height, minimum, maximum or basis is relative, is
  tainted (laid out at a height, it spreads free space or resolves the child
  against it).

- `src/tree/cache.rs`: `Taint`, `Cache::get_tainted`, `Cache::store_tainted`;
  `answers` relaxes an axis only for an untainted entry. `Cache::store` (other
  trees) stores fully tainted entries, which answer only their own space.
- `src/tree/taffy_tree.rs`: `TaffyView::compute_child_layout` caches with the
  taint it works out and passes width taints up.

Remove this part once upstream compares the available space there too.

## A node styled for the width it is solved at

Some layouts change with the width they get: a form's label stands beside its
control or above it, a grid has as many columns as fit, a board's lists share
the row. Upstream can only learn that width from a finished solve, so such a
node was styled after the solve and the tree solved again, up to once per
decision in the window.

A node marked with `TaffyTree::set_restyled_for_width` is handed to a
`RestyleFunction` each time it is solved and its cache does not answer, before
its algorithm reads its style: with its border-box width when its parent
decided it, else the space it may take. The function gets a `Restyle`, which
reads the node's and its children's styles and where the last solve put them,
and sets them. The node may change only what its own algorithm reads (its
tracks, gaps, alignment), since its parent has read the rest; a child may
change any of its style, and its cache is cleared when it does.

The function is called a second time once the node's algorithm laid it out
(`Solving::Laid`), with what that layout answered and where each child landed:
a view that learns only from its laid content whether it overflows sets its
gutter there, and a list places the rows it just measured. When the function
answers true the node is laid out again, at most twice. Its parent recorded
the node's padding and border before this, so a node that changes them reads
them back from its style.

- `src/tree/taffy_tree.rs`: `NodeData::restyled_for_width`,
  `TaffyTree::set_restyled_for_width`,
  `TaffyTree::compute_layout_with_measure_and_restyle`, `Restyle` (with
  `parent` and `laid`), `Solving`, `RestyleFunction`;
  `TaffyView::compute_child_layout` calls the function on a cache miss, before
  and after the node's algorithm.
- `src/tree/mod.rs`, `src/lib.rs`: export `Restyle`, `RestyleFunction` and
  `Solving`.

Remove this part once upstream lets a node choose its style from the size it
is offered within a solve.

## A node that has left the tree marks nothing

`TaffyTree::mark_dirty` indexed `nodes[node]`, so marking a node retired between
the build that gave out its id and the frame that marks it (a widget that left
while a surface was being laid out) panicked inside taffy. A node the tree no
longer holds is skipped: it has no layout to forget, and a retired widget must
never take a frame down.

- `src/tree/taffy_tree.rs`: `mark_dirty` returns early when the node is not in
  the tree.
