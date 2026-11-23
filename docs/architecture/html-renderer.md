# Widgets as HTML

A workspace's apps open as web pages at `/w/<workspace>/<app>`
(`crates/server/tilo-server/src/web/page.rs`). The page is not a picture of a
painted frame. Every widget renders itself as HTML, next to its paint: the same
widgets, the same composition and the same apps as on native, with real elements
where a window has pixels. So the page has what a page should have: real text
that the browser lays out, selects and finds, `<input>` fields that autofill and
take passkeys, native scrollers, buttons and list items that assistive
technology reads, and CSS layout that follows the browser window as it resizes.

## The contract

`Widget::html(&self, cx: &mut HtmlCx)` (`crates/ui/ui-core/src/composition.rs`)
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
  video) calls `cx.picture()`: the page holds a `<canvas>` island for its
  paint, marked with the paint's count (`data-painted`). The server page does
  not fill these islands yet.
- A **control** calls `cx.control(control)`: the element the browser and an
  assistive technology know, holding what the widget holds. The page builds it
  from `Control`, and what the person does on it comes back as the widget's own
  semantic action (`semantic`, the path a field already uses), the same input
  path a native window's accessibility and automation take.

The trait's default, `HtmlCx::from_layout`, renders a widget from its layout
node and its role (its children, or a picture for a leaf). It stands in for
nothing: `tools/policy/check_widget_html.py` refuses an `impl Widget` without
`fn html`, so every widget says what it is on a page.

## Controls

`Control` (`crates/ui/ui-core/src/composition.rs`) is one kind per family of
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
no `value` is only its looks, and the page does not act on it.

Icons reach a page through `composition::glyph`, as SVG outlines in the icon's colour.

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
paints, so the island says when its picture changed. The controls' own
elements are listed above.

A widget shows what it paints itself from parts of its own: `cx.words` puts a
run of text as an element (`h3`, `dt`, `td`, `li`) access where the widget
sets it, `cx.attr` gives its element what only its kind carries (an `<img>`'s
`src` and `alt`, a `<progress>`'s `value`, a `<time>`'s `datetime`), and
`cx.ground` fills its element with the ground the widget paints under it (a
card's, a badge's), so a page reads like the native window in light and dark.

## How a page is served

The browser loads a small document from the address: a stylesheet and the
page's script (`crates/server/tilo-server/src/web/page/page.mjs`), no app code.
The script proves the browser's device on a WebTransport session and asks for
the page. The workspace service opens the app's view for that member in an app
worker (`crates/agent/agent-session/src/workspace_actions/pages.rs`), which
lays it out headlessly. `FrameHarness::composition()`
(`crates/ui/ui-core/src/headless/composition.rs`) walks the laid-out tree from
its root and asks each widget node for its `html`; layout-only nodes become
plain containers with their style. The result (`ui::composition::Composition`)
is platform-neutral.

`ui::composition::page::Page` (`crates/ui/ui-core/src/composition/page.rs`)
turns it into the page's markup, which carries no geometry of the frame it was
composed in: a container is a CSS flex or grid box with its node's style, so the
page lays out at any width and the server never needs the window's size. Every
element the page acts on carries `data-p`, a number `Page` keeps for the
widget's path from one render to the next. `Page::render` answers the whole
body; `Page::patch` answers only the elements that changed, each replacing the
element with its number.

What the person does comes back as the widget's semantic action: `data-a` names
it (`activate`, `toggle`, `value`, `step`, `edit`), `data-t` the semantic target
and `data-v` the value it sets. The script sends one `PageAsk::Act` per click or
edit (`tilo_server_protocol::page`), the worker acts on the view as a click or
typing in a window would, and the answer is the patches. An open page also asks
every few seconds what others changed, so their edits reach it.

## Tests

The composition is tested where the widgets and apps are:
`crates/ui/ui-core/src/composition/page.rs`'s own tests, and the server page's flow in
`crates/server/tilo-server/src/web/page/tests.rs` and
`crates/server/tilo-server/src/web/page/page.test.mjs`.
