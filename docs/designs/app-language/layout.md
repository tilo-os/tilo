# Layout

The obvious way to write a view gives a modern layout that adapts to every window size, DPI,
aspect ratio and text size. Getting it wrong should take effort. The engine underneath is Taffy
(flexbox and grid); this doc defines the model views are written against.

## 1. Rules

1. **Constraints go down, sizes come up, the parent places.** A child never reads its own final
   size to change its content, so layout never oscillates.
2. **Adapt to the space you're given, not the screen.** A card behaves the same in a library, a
   narrow strip, a dashboard cell and a chat message.
3. **As big as the content by default.** Nothing has a fixed pixel size unless it asks.
4. **No pixel numbers in apps.** Gaps come from the design system's rhythm tokens; content sizes
   are in `rem`, relative to text size, so large text reflows instead of clipping.
5. **Adaptive patterns are primitives:** sidebar, list and detail, inspector, card grid, readable
   page. Nobody hand-writes breakpoints for them.
6. **Layout changes animate:** nodes are keyed, so items move and resize on the platform's
   springs instead of jumping.

## 2. Units and sizes

- Units: `rem` (body text size, after the person's setting), `dp` (a logical pixel, rarely
  needed), rhythm tokens (`tight`, `related`, `sibling`, `section`, `group`) for every gap, `fr`
  for shares of space.
- Per axis: `hug` (default), `fill` or `fill(2)`, `min:` and `max:`, and `aspect:` (e.g. `16/9`).
  No absolute positions; overlays use `layers`.

## 3. Primitives

| Primitive | Does |
|---|---|
| `stack` | children top to bottom |
| `row` | children leading to trailing (mirrored right-to-left); `wrap: true` flows chips |
| `grid(min: 11rem)` | as many equal columns as fit: one on a phone, five on a monitor; `span: 2` |
| `split` | two panes side by side when both fit, otherwise collapsed: stacked, a drawer, or one at a time with back |
| `page` | a readable width (about 40rem), centered; `page(full)` for tables, maps, media |
| `layers` | children on top of each other, each placed at one of nine positions |
| `scroll` | scrolls on its axis |
| `spacer` | takes leftover space |

Widgets own their padding and size, as the design system says. Text wraps by default; `lines: n`
clamps it.

## 4. Responding to space

The primitives do most of it. For the rest, a view reads `space`, what its container offers:

```
view player_bar(p: Player) {
    if space.compact {
        row(distribute: between) { now_playing_small(p); play_pause(p) }
    } else {
        row(gap: section) { now_playing(p); spacer; transport(p); volume(p) }
    }
}
```

- Size classes in `rem`: `compact` (< 30rem), `medium` (< 52rem), `expanded` (< 75rem), `wide`.
  Large text moves a window into a narrower class, which is right.
- Also `space.orientation` (portrait, landscape, ultrawide) and, rarely, `space.width` in `rem`.
- A view re-runs only when a class it read changes, not on every pixel of a resize. Boundaries
  have a little hysteresis so a window resized right at one doesn't flicker.
- Each view sees its own container's space: a card in a grid cell sees the cell.

## 5. Every screen

Logical units with edges snapped to device pixels (layout rounds every edge to the display's
pixel grid, at fractional scales too); layouts reflow at 200 % text; sensible on portrait, phone
and ultrawide; hit targets never shrink; leading and trailing instead of left and right.

## 6. Checking

`tilolang layout-check` renders every view headlessly across sizes from 320×568 to 3440×1440,
display scales 1, 1.5 and 2 and text scales 1 and 1.5. It reports overflow, clipped text, overlaps,
hit targets that are too small, pages that scroll sideways and edges between device pixels, each at
a view path. Agents run it like the type checker.

## 7. On the platform

The primitives are shared ui-core layout widgets (`Stack`, `Row`, `Grid`, `Split`, `Page`,
`Layers`, `Scroll`), usable from Rust apps too, built on Taffy. `split` decides its arrangement
during layout, so collapsing needs no re-evaluation.

## 8. Example

```
view album_page(a: Album) {
    page(full) {
        split(side: leading) {
            primary(min: 18rem) { album_art(a); album_facts(a) }
            secondary(min: 22rem) { scroll { for t in a.tracks { track_row(t) } } }
        }
    }
}
```

Side by side on a wide window, stacked on a narrow one, with no breakpoints.
