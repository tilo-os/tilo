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
  needed), rhythm tokens (`tight`, `related`, `sibling`, `section`, `group`, `none`) for every
  gap and every padding, `fr` for shares of space.
- Per axis: `hug` (default), `fill` or `fill(2)`, `min:` and `max:`, and `aspect:` (e.g. `16/9`).
  No absolute positions; overlays use `layers`.
- The fit is the view's, wherever it is put: a view that says `fill` fills the slot it is in,
  for a component with a body slot as for a `stack`, `row`, `grid` or `page`. A card's body, a
  window's content and a nav shell's column hold their child at the fit its node wrote, so a
  mailbox or a note placed in a place is the height of the window, never one line. The host
  carries that fit into the component (`ui.widget::Fitted`); the component's slot takes it, so
  no app or shell writes a height to make it so.

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

Widgets own their padding and size, as the design system says. A container that holds what it did
not draw — a desk's surfaces, a panel's body — keeps a padding of its own (`padding: .section`):
the room it holds back on every side. Text wraps by default; `lines: n` clamps it.

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
- The toolkit's own rule for a sidebar follows from the classes: a window stands its sidebar beside
  the content only while the pane beside it keeps the class the content's kind asks for there
  (`ui::widget::ContentKind::beside_sidebar_class`) — a column of text is no phone's screen, so it
  asks for `medium` (`ui::widget::arrange::column_min_width`), where a grid of tiles or a surface
  drawn on works narrower. Below that class the sidebar folds behind the bar's toggle and the
  content takes the window. The window's own width decides it, never the display's: a window in a
  strip, a tiled window and a phone follow one rule.
- Also `space.orientation` (portrait, landscape, ultrawide) and, rarely, `space.width` in `rem`.
- A view re-runs only when a class it read changes, not on every pixel of a resize. Boundaries
  have a little hysteresis so a window resized right at one doesn't flicker.
- Each view sees its own container's space: a card in a grid cell sees the cell.

## 5. Every screen

Logical units with edges snapped to device pixels (layout rounds every edge to the display's
pixel grid, at fractional scales too); layouts reflow at 200 % text; sensible on portrait, phone
and ultrawide; hit targets never shrink; leading and trailing instead of left and right.

A page keeps one content gutter, `rhythm.group`, at its sides and its ends at every width: what a
page holds stands one gutter in from the column, the same gutter separates it from what stands
over and under it, and every app's page keeps the same one, so a notice over a list has as much
room above it as below at a phone's width and a monitor's alike.

## 6. Checking

`tilolang layout-check` renders every view headlessly across sizes from 320×568 to 3440×1440,
display scales 1, 1.5 and 2 and text scales 1 and 1.5. It reports overflow, clipped text, overlaps,
hit targets that are too small, pages that scroll sideways and edges between device pixels, each at
a view path. Agents run it like the type checker.

It also reads what a screen says and how its rows line up: every row at one depth starts its title
at one x, nesting indents by one gutter, a box's rows end at one right edge, one choice is drawn
selected once, a list offers one add entry, a name a screen reader announces is words rather than a
code identifier, a destructive control says "Delete", a surface holds no surface of its own kind,
a narrow window keeps a 16 px gutter, a list beside the page it opens highlights the row that page
shows, a section's heading keeps clearly more room under the block over it than over the content it
opens, and a surface whose corners round — a card, a notice — keeps a gutter from the edge of the
column or window it stands in, where an edge would cut its shape, and the gap of a sibling from what
stands over and under it, so a list under a notice is not read as a row of it. It reads that an item
stands once: a list folds the branch of a row standing open into its item, since the document under
that row lists it, so no step of a task is read both there and again as a row of the list.

It reads a window as it stands at rest too, its bar hidden over the window's top edge, and holds a
sidebar that declares a header row — the words that name its places and the buttons that add to
them — to drawing it there: what a sidebar says of itself is on the window at every time, never
only once the person reaches for the bar (`sidebar-header-on-reveal`).

It reads a field's line where the field stands it: a field one band tall keeps its one line in the
middle of the control, even where that band grew to a finger's target or a row stretched it to the
face of the button beside it, while text that wraps, a box given rows of its own, a document's page
and a field stretched to fill a pane start at the inset and grow down.

It reads the words a window shows where their ink stands, the bar's own painted labels among
them — the words of its tabs, its location, its pickers and its notices: two words drawn in one
place are a `text-on-text` finding unless a surface of one's own stands between them, since
whatever a person reads there is one run of letters, not two.

It also reads the language a screen speaks: a window has one, so every word it says must resolve in
that language, and a word only another language's catalogue holds is a `mixed-language` finding
(i18n.md §4). `--locale de-DE` stages the person's language for the check, whose default is `en-US`
whatever the machine's own locale is, so a check reads the same everywhere.

`--key Tab`, `--key Down`, `--key Space` press keys into a window before it is read, in the order
given and at the `--dump` size, so a check and a shot show the layout a person reaches by keyboard
rather than only its first frame.

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
