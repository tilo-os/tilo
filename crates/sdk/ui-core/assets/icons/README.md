# Tilo icons

One SVG per icon, named by its file: `layout-grid.svg` is `Icon::LayoutGrid`, and data that cannot
name the enum (a manifest, a search row, tilolang's `icon: .layout_grid`) uses the kebab name.
`build.rs` turns these files into the toolkit's icon table, so the native renderer and the web page
draw the same outlines. Nothing else holds icon geometry.

## Drawing one

- A 24 × 24 grid, drawn inside the 2-unit margin (2..22) unless the shape needs the edge.
- One stroke: 1.5 px, round caps, round joins, in the ink (`currentColor`). The toolkit keeps the
  stroke 1.5 px at every size, so a 16 px icon is as crisp as a 24 px one.
- Corners: 2-unit radius on rectangles a hand would hold (a window, a phone, a card), 1 on small
  cells (a grid's squares); a stroke ends round.
- Shapes are `path`, `rect`, `circle`, `ellipse` and `line`, with only their geometry attributes.
- A filled state is its own icon (`heart-filled`); a mark drawn in fills (`tilo`, `discord`) says
  `fill="currentColor"` on its root. The build refuses any other root, so an icon on another grid,
  weight or corner never enters the set.
- `<desc>` documents the icon where its name doesn't say enough; it becomes the variant's doc.

## Where they come from

Most icons are drawn from Lucide 0.546.0 (`lucide-static`), re-rooted to the stroke above; Lucide is
ISC licensed, with portions from Feather under MIT (`LICENSE-Lucide.txt`, which must ship with
them). `tilo` is the Tilo mark. `discord` is Discord's mark, drawn as Discord publishes it for
linking to its service. Icons Tilo draws itself follow the rules above and need no notice.
