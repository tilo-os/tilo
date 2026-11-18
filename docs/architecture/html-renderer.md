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
- A widget that is **genuinely drawn** (a chart, a canvas, a keyboard map,
  video) calls `cx.picture()`: the page shows its paint as a `<canvas>` at the
  screen's density, asked for when what it shows changed.
- A **control** calls `cx.control(control)`: the element the browser and an
  assistive technology know, holding what the widget holds. The page builds it
  from `Control`, and what the person does on it comes back as the widget's own
  semantic action (`semantic`, the path a field already uses): one input path
  for the canvas and the page alike.

The trait's default, `HtmlCx::from_layout`, renders a widget from its layout
node and its role (its children, or a picture for a leaf). It exists only for
widgets not yet given their own rendering. `tools/policy/check_widget_html.py`
refuses an `impl Widget` without `fn html` beyond the counts listed in
`tools/policy/widget_html_baseline.txt`, and asks for a count to be lowered
once a widget gets its own, so the list only shrinks and new widgets cannot
skip it.

## Controls

`Control` (`crates/sdk/ui-core/src/composition.rs`) is one kind per family of
control, carrying what the element holds:

| Kind | The page's element | What comes back |
|---|---|---|
| `Check` | a `<label>` around `<input type=checkbox>` (a radio's in a set, a switch's as `role=switch`), with its words | `ToggleCheck` |
| `Range` | `<input type=range>` with `min`, `max`, `step` and the app's words for the value | `SetValue` |
| `Entry` | `<input>` of a kind (`number` with two steppers, `date`, `time`, `colour`, `search`) | `SetValue` |
| `Select` | a `<button role=combobox aria-haspopup=listbox>` showing the current value, and its open `role=listbox` of `role=option`s | `Activate` to open; `SetValue` per option |
| `Choices` | a set (`tablist`, `radiogroup`, `menu`, `toolbar`, `group`) of `role=tab`/`radio`/`menuitem`/`button` choices, each naming its semantic target | `Activate` on that target |
| `Link` | `<a href>` with its words | `Activate` |

A widget that shows several parts of one control (a colour's swatches beside
its hex field) calls `cx.control` once per part. A choice with no `target` and
no `value` is only its looks: a press on it goes to the canvas as every other
press does, and the widget acts by where the press landed.

Icons reach a page through `composition::glyph`, which is part of `HTML_CODE`
with the faces, so an SVG outline's path data is not in a program that never
composes a page.

Widgets with their own HTML so far: `Text`, `EditableLabel`, `BlockEditor`,
`Button` (a `<button>` whose face is its icon, words and state), `ListRow`,
`Column`, `Row`, `Stack`, `Container`, `Scrollable`, `VirtualList`, `NavShell`
and its header title, `ModalDialog` and its footer actions (buttons with their
words), `OverflowButton`; the composition primitives (`Spacer`, `When` and the
`Arranged` containers, `Router`, `UiGraph`'s live UI, the app root binding, the
hosts that hold one widget) are their children and the container's own style,
never a picture; the `ActionPalette` is a `role=dialog` with a real search
`<input>` and a `role=listbox` of options, each a row with its icon, its words
and its shortcut; the window's search is its field and its buttons; a floating
window is its title, its close button and the content it holds; the window
strip's tiles, groups and their controls are their children and their glyphs;
the history host and the framework's presentation compose their children, and
the header a client-drawn window draws is a `<header>`. The content widgets
show what they are: `DataTable` (a `<table>` of `<th>` and `<td>`),
`PropertyList` and `Facts` (a `<dl>` of terms and descriptions), `Breadcrumb`
(a `<nav>` holding an `<ol>`), `ConversationList` and `ActivityTimeline` (a
list of lines with their authors and times), a quote (a `<blockquote>`),
`CodeEditor` (a `<pre><code>` to read, its own field to edit), `ProgressBar`
(a `<progress>` with its value), `Gauge` (its name and readout as words over a
track and fill the page lays), the `Section` family (a `<section>` with its
heading, count and action), `EmptyState`, `Banner` (a live region), `DropZone`,
`SearchResults`, `Card`/`MediaCard`/`StatTile`/`QuickTile` and their grids,
`TextStack`, `Badge`, `Avatar` (an `<img>` where its address loads, its
initials as text otherwise), `Image` (an `<img>` where its address loads),
`Icon`, `Divider` (a `<hr>`), `Spinner` and `Skeleton` (drawn by the page's own
CSS), `EmojiPicker`, `ReactionStrip`, `MediaTransport`'s art, lines and times,
`PagePreview`, `PageSidebar`, the `Surfaces` components, and the containers
that only place what they hold (`GroupRow`, `Columns`, `DragRun`,
`GraphCanvas`, `Manual`, `DocumentView`, `DocumentViewport`, `DisclosureRow`);
a widget that is genuinely drawn (`Chart`, `Sparkline`, `Plot`, `Map`,
`Drawing`, `QR`, `VideoPlayer`, `ImageViewer`, `ImageCrop`, `ProgressRing`)
shows as its picture inside an element that carries its name. What a widget
draws itself — a web view, a native surface mounted by the platform, an
embedded app, a capture's canvas, a display placement, a split's rule — is a
picture of that paint, with the controls it names (`HtmlCx::targets`) as real
buttons over it: `HtmlCx::painted` takes the count a widget bumps each time it
paints, so the page fetches a new picture when there is one. The controls' own
elements are listed above. The window's bar of slots, which the frame hosts
over a page that mounts one app, is a `<header>` toolbar of real buttons over
its painted band.

A widget shows what it paints itself from parts of its own: `cx.words` puts a
run of text as an element (`h3`, `dt`, `td`, `li`) access where the widget
sets it, `cx.attr` gives its element what only its kind carries (an `<img>`'s
`src` and `alt`, a `<progress>`'s `value`, a `<time>`'s `datetime`), and
`cx.ground` fills its element with the ground the widget paints under it (a
card's, a badge's), so a page reads like the canvas in light and dark.

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
  widget as its value through the semantic action API.
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
`create-page` action:

| | 1280 px | 390 px |
|---|---|---|
| typing, keydown to the page showing it, p50 / p90 | 41 – 52 / 86 – 89 ms | — |
| elements in the window | 35 | 27 |
| presenter time per frame | 0.4 ms | 0.4 ms |

Typing in an unoptimized build is dominated by layout in wasm; the release
bundle's numbers come next.

## Next

- Give the remaining widgets their own `html`, starting with what the shell
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

A tilolang app runs on the web through the same mount: the page installs the
package from the address its payload is served at (`?package=tl/<app-id>/`,
`installTilolangPackage`), and the window runs the package's instance in the
page. One command builds the gallery on the build machine, fetches it and
serves it here, printing the page that shows the canvas and the HTML side by
side (or one at a time, in light or dark):

```sh
tools/web/html-renderer/tilolang-web.sh            # apps/tl/gallery
tools/web/html-renderer/tilolang-web.sh apps/tl/tasks --search apps/tl
```

`tilolang-check.mjs` works the same page in headless Chromium on the build
machine: a screenshot of each renderer in light and dark, then a field typed
into and a control pressed in each.
