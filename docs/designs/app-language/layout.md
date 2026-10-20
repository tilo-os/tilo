# tilolang layout

Layout is the part of a UI framework that can't be fixed later, so this document settles it before
any widgets are built. The goal: the obvious way to write a view produces a modern layout that
adapts to every window size, DPI, aspect ratio and text size. Getting it wrong should take effort.

Tilo's current toolkit is not the model here. Its layout gets reused only where it already matches
this design. The engine underneath is Taffy, which does flexbox and CSS grid well; the **model**
apps write against is what this document defines.

## 1. Principles

1. **Constraints flow down, sizes flow up, the parent places.** A parent tells each child how much
   space it may use. The child picks its size within that and reports it. The parent positions it.
   No child reads its own final size to change its content, so layout never oscillates. This is
   Flutter's rule, and it keeps every case predictable.
2. **Responsive to the container, not the screen.** A view adapts to the space it is *given*.
   An album card behaves the same way in a full-screen library, a narrow strip, a dashboard cell
   and a chat message. Nothing reads the screen size. That is what makes views composable, and
   Tilo windows are strips of arbitrary width, so screen size would be meaningless anyway.
3. **Intrinsic by default.** Things are as big as their content (*hug*) unless they ask for more.
   Nothing has a fixed pixel size by default.
4. **No pixel numbers in apps.** Spacing comes from the design system's rhythm tokens. Sizes that
   relate to content are given in `rem`, relative to the text size, so a layout reflows when the
   user enlarges text instead of clipping it. Logical units absorb DPI; apps never see device
   pixels.
5. **Adaptive patterns are primitives.** Sidebar, master and detail, an inspector, a responsive
   card grid, a readable page: each of these is a single primitive with built-in behaviour for
   narrow and wide space. Apps don't hand-write breakpoints for them.
6. **Layout changes animate.** Nodes are keyed, so when a layout rearranges (a split collapses, a
   grid gains a column), items move and resize on the design system's springs rather than jump.

## 2. Units and sizing

**Units:**

| Unit | Means | Use |
|---|---|---|
| `rem` | The base body text size, after the user's text-size setting. | Minimum widths, readable widths, content-related sizes. |
| `dp` | One logical pixel, after display scale. | Rare: hairline-adjacent content, canvas sizes. |
| Rhythm tokens | `tight`, `related`, `sibling`, `section`, `group`, from the design system. | Every gap and padding. |
| `fr` | A share of remaining space. | Grid tracks and weighted fill. |

**Sizing**, per axis, on any node:

| Mode | Means |
|---|---|
| `hug` (default) | As big as the content, within the constraints. |
| `fill` / `fill(2)` | Take the remaining space, weighted. |
| `min:` / `max:` | Bounds in `rem` or `dp`. |
| `aspect:` | A fixed width/height ratio, e.g. `aspect: 16/9` for media. |

There is no absolute positioning and no fixed width by default. `layers` (§3) covers overlays.

## 3. Primitives

There are eight layout primitives. Together they cover essentially every modern app layout.

| Primitive | Behaviour |
|---|---|
| `stack { }` | Children top to bottom. `gap:` (default `sibling`), `align:` cross-axis (start, center, end, stretch; default stretch). |
| `row { }` | Children left to right, mirrored in right-to-left locales. `gap:` (default `related`), `align:` (default center), `distribute:` (start, center, end, between). `wrap: true` turns it into a flowing row for chips and tags. |
| `grid(min: 11rem) { }` | **The responsive grid.** As many equal columns of at least `min` as fit; items fill them. One column on a phone, five on a monitor, with no breakpoints. `grid(columns: 3)` gives a fixed count. Children may `span: 2`. |
| `split { primary {} secondary {} }` | **The adaptive two-pane pattern** for sidebars, master and detail, and inspectors. Side by side when both panes' `min:` fit. Otherwise it collapses by its `collapse:` mode: `stack` (default: secondary under primary), `drawer` (secondary becomes a drawer the user opens) or `navigate` (one pane at a time, with a back affordance). `side: leading/trailing` and `ratio:` set which pane is the sidebar. |
| `page { }` | The root of a document-like view. Content is bounded to a readable width (`measure`, about 40rem) and centered. The page gutter scales with the space class. `page(full)` drops the bound, for canvases, tables, maps and media. |
| `layers { }` | Children drawn on top of each other: badges, a play button over album art, a floating action. Each child has an `at:` (one of nine positions). |
| `scroll { }` | Scrolls on its axis (default vertical). Inside a scroll the constraint on that axis is unbounded, so children hug. |
| `spacer` | Takes leftover space in a `row` or `stack`. |

Widgets (text, buttons, inputs, list rows, cards) live inside these primitives and own their own
padding and size, as the design system already prescribes. Text wraps by default. `lines: n`
clamps it, and `truncate` ellipsizes it.

## 4. Responding to space

Most adaptation comes from the primitives: `grid` reflows, `split` collapses, `page` bounds its
width. For the remaining cases a view can read **`space`**, the constraint its container gives it:

- `space.width_class`: `compact` (< 30rem), `medium` (< 52rem), `expanded` (< 75rem), `wide`.
  Classes are in `rem`, so a large text size moves a window to a narrower class. That is exactly
  right, because there is less room for text.
- `space.compact`, `space.wide`: shorthands for the class tests.
- `space.orientation`: `portrait`, `landscape` or `ultrawide` (ratio > 2.2).
- `space.width` and `space.height` in `rem`, for the rare case that needs a number. Prefer the
  classes.

```
view player_bar(p: Player) {
  if space.compact {
    row(distribute: between) { now_playing_small(p); play_pause(p) }
  } else {
    row(gap: section) { now_playing(p); spacer; transport(p); volume(p) }
  }
}
```

**How it works.**

- `space` is the *available* constraint offered by the parent, not the view's resulting size.
  Constraints flow down, so there is no cycle.
- The runtime records reads of `space` like any other read (implementation.md §4.5). A view
  re-evaluates only when a value it read changes. A view that read `space.compact` re-runs when
  the width crosses the 30rem class boundary, not on every pixel of a resize.
- Class boundaries have a small hysteresis band, so a window resized exactly at a boundary doesn't
  flicker between layouts.
- Each view call is a boundary with its own `space`. An `album_card` inside a `grid` cell sees the
  cell's width, not the window's.

**Order of evaluation.**

1. The host evaluates a view with the `space` from the last layout. On first display, it uses its
   parent's offer.
2. It lays out the result.
3. Where a view boundary's offered constraint changed class, it re-evaluates that subtree once and
   lays out again.

This is bounded: at most one extra pass per boundary per frame, which in practice almost never
happens.

## 5. Every screen

These are the requirements every layout must meet:

- **DPI.** Everything is in logical units. The renderer snaps edges to device pixels, and hairlines
  are one device pixel. Images choose their resolution from the scale factor.
- **Text size.** `rem`-based minimums and readable widths mean that 200 % text reflows (fewer grid
  columns, a split that collapses) instead of clipping.
- **Aspect ratio.** `space.orientation` handles portrait monitors, phones and ultrawides. `page`
  and `split` behave sensibly on all of them by default.
- **Touch and pointer.** Hit targets come from the widgets' control heights. Layouts never shrink
  them.
- **Right-to-left text.** `row`, `split` sides and `layers` positions are logical (leading and
  trailing), never left and right.

## 6. Layout checking (tooling, part of v1)

Layout bugs are invisible until someone resizes the window. So:

- `tilolang layout-check <package>` renders every view headlessly across a matrix and reports
  failures as diagnostics, at a view path and node key:
  - **Sizes:** 320×568, 390×844, 768×1024, 1024×768, 1280×800, 1920×1080, 2560×1440, 3440×1440 and
    1080×1920.
  - **Display scales:** 1, 1.5 and 2.
  - **Text scales:** 1 and 1.5.
  - **Failures:** overflow, clipped text, overlapping siblings, hit targets below the minimum,
    horizontal scrolling of a page.
- Agents run it like the type checker and get a precise list to fix. The same matrix runs in
  tests for every built-in package.

## 7. Mapping onto the platform

| Primitive | Implemented as |
|---|---|
| `stack`, `row` | A Taffy flex container: direction, gap from the token, align and justify. `wrap` maps to flex-wrap. |
| `grid(min:)` | A Taffy grid with `repeat(auto-fill, minmax(min, 1fr))`. `columns: n` maps to `repeat(n, 1fr)`; `span` maps to a grid-column span. |
| `split` | A host layout node that decides its arrangement *during layout* from its offered width. Both panes exist, so no re-evaluation is needed. It switches between a flex row and its collapse mode; the `drawer` and `navigate` modes use the platform's drawer and navigation primitives. |
| `page` | A container with max-width `measure` and auto margins, plus a gutter from the class. |
| `layers` | A container whose children share one cell, each with its own alignment. |
| `scroll` | ui-core's scroll view. |
| `space` | Computed from the offered width of each view boundary node, after Taffy's pass. Fed to the runtime as an input the read recorder tracks. |

These primitives are **shared ui-core layout widgets** with a semantic API (`Stack`, `Row`, `Grid`,
`Split`, `Page`, `Layers`, `Scroll`). They are not tilolang-private, so Tilo's Rust apps can adopt
the same model. tilolang renders onto them directly. If the UI graph's node set (`Column`/`Row`
with a gap) can't express them, the host renders its keyed tree straight onto ui-core widgets,
with the same keyed reconciliation.

## 8. Examples

**A responsive library:**

```
view library(l: Library) {
  page {
    heading("Albums")
    grid(min: 11rem) {
      for a in l.albums { album_card(a) }
    }
  }
}
```

**Album detail: side by side on a wide window, stacked on a narrow one:**

```
view album_page(a: Album) {
  page(full) {
    split(side: leading) {
      primary(min: 18rem) { album_art(a); album_facts(a) }
      secondary(min: 22rem) {
        scroll { for t in a.tracks { track_row(t) } }
      }
    }
  }
}
```

**A card that adapts to its own cell:**

```
view album_card(a: Album) {
  layers {
    image(a.cover, aspect: 1)
    play_button(a, at: bottom_trailing)
  }
  text(a.title, lines: 1, truncate)
  if not space.compact { caption(a.artist) }
}
```

**A chat message containing an agent's block:** the block is a view boundary, so it sees the
message column's width and adapts with no extra code.
