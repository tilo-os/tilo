# Widgets as HTML

On the web, a Tilo window can show as a WebGPU canvas or as HTML. The HTML is
not a copy of the painted frame. Every widget renders itself as HTML, next to
its paint: the same widgets, the same composition and the same apps as on
native, with real elements where the canvas has pixels. The page then gets
what a page should have: real text that the browser lays out, selects and
finds, `<input>` fields that autofill and take passkeys, native scrollers,
buttons and list items assistive technology reads, and CSS layout that follows
the window as it resizes. Pixels may differ from the canvas; what the window
shows and does is the same.

It is on main behind `mountDesktop(el, {app, dom: true})`. A page chooses the
renderer by platform and readiness in code, never from its address.

## The contract

`Widget::html(&self, cx: &mut HtmlCx)` (`crates/sdk/ui-core/src/composition.rs`)
is part of the widget contract, beside `draw`:

- A **container** composes its children: `cx.children()` puts the widgets
  its layout node holds into its element. Where they stand where the node's
  style puts them, the element is a CSS flex or grid container with that
  style (`flows`), and each child is a flex item at the size the shared
  layout gave it. A widget that places its children itself (insets, a shell
  that docks and floats panes) has them placed at their rectangles. Surfaces
  it lays under them (a sidebar's ground) are `cx.surface(rect, "sidebar")`.
- A **leaf** names its element and what it shows: `cx.element("li")`,
  `cx.text(look)` for words set the way the widget sets them (size, weight,
  leading, colour, line clamp), `cx.field(…)` for an editable field (an
  `<input>` or `<textarea>`), `cx.blocks(…)` for a document of blocks
  (paragraphs, headings, list and checklist items, quotes, code, with inline
  marks and the caret).
- A widget that is **genuinely drawn** (a chart, a canvas, a map, video)
  calls `cx.picture()`: the page shows its paint as a `<canvas>` at the
  screen's density, asked for when what it shows changed.

The trait's default, `HtmlCx::from_layout`, renders a widget from its layout
node and its role (its children, or a picture for a leaf). It exists only for
widgets not yet given their own rendering. `tools/policy/check_widget_html.py`
refuses an `impl Widget` without `fn html` beyond the counts listed in
`tools/policy/widget_html_baseline.txt`, and asks for a count to be lowered
once a widget gets its own, so the list only shrinks and new widgets cannot
skip it.

Widgets with their own HTML so far: `Text`, `EditableLabel`, `BlockEditor`,
`Button` (a `<button>` whose face is still its picture), `ListRow`, `Column`,
`Row`, `Stack`, `Container`, `Scrollable`, `VirtualList`, `NavShell` and its
header title, `ModalDialog` and `OverflowDoor`. The window's bar of slots,
which the frame hosts over a page that mounts one app, is a `<header>`
toolbar of real buttons over its painted band.

## How a frame reaches the page

`FrameHarness::composition()` walks the laid-out layout tree from its root and
asks each widget node for its `html`; layout-only nodes become plain
containers with their style. The result (`ui::composition::Composition`) is
the frame's composition, platform-neutral. The browser runtime hands it to the
page as JSON for each frame that changed it (`src/browser/dom.rs`,
`src/dom_code.rs`), and `web/view-present.mjs` reconciles it into elements by
widget path, writing only the properties that changed.

Input:

- The pointer stays the runtime's, as on the canvas: `web/dom-input.mjs`
  forwards it, so a press, a hover or a drag does what the widget does.
- A press in a document's text puts the caret where the browser laid that
  text out: the page reads the offset at the point, and the block editor
  receives it as a semantic selection on the block (`block-text:` and its
  id). Keys then reach the editor as they do on the canvas, and the edit
  comes back in the next frame's composition.
- A field is the page's own `<input>`: what the person types goes to the
  widget as its value through the semantic action door.
- A scroller is native: the page scrolls it, and the offset goes back to the
  widget (`dom_scrolled`), which composes what came into view.
- Pictures are asked for after the frame was handed over (`dom_picture`);
  the runtime rasterizes the widget's own paint on the CPU at the page's
  device pixel ratio.

The code only an HTML mount runs (the presenter and the pictures'
rasterizer) is behind `DOM_CODE` (`ui::lazy_code!`), so the desktop bundle
fetches it the first time a page asks for `dom: true`.

## Measurements

A development build of the Notes bundle (`wasm-dev`, unoptimized), headless
Chrome on a laptop, with a note made through Notes' own
`import-markdown-content` action:

| | 1280 px | 390 px |
|---|---|---|
| typing, keydown to the page showing it, p50 / p90 | 41 – 52 / 86 – 89 ms | — |
| elements in the window | 35 | 27 |
| presenter time per frame | 0.4 ms | 0.4 ms |

Typing in an unoptimized build is dominated by layout in wasm; the release
bundle's numbers come next.

## Next

- Give the remaining widgets their own `html`, starting with what the desk
  and Chat use, and lower the baseline as each lands.
- Buttons and list rows: their faces as HTML (label, icon, state) rather
  than a picture.
- Retire the semantic mirror for HTML mounts: the rendered elements already
  carry each widget's role, name and state, so focus can move onto them.
- Remove the scene keying the paint-frame renderer needed (keyed marks in the
  encoding, glyph runs carrying their characters, the drawn scroll and field
  lists), which nothing reads any more.

## Reproducing

```sh
# on a build machine, from a worktree of main; fetch the results back after
tools/web/html-renderer/build-notes-web.sh   # the release Notes bundle with app.html
```

`app.html` mounts one app of the bundle as HTML; `?render=gpu` mounts it on
the WebGPU canvas to compare.
