# Views as HTML

A workspace's apps open as web pages at `/w/<workspace>/<app>`
(`crates/core/tilo-server/src/web/page.rs`). The page is not one picture of a
painted frame. It is written from the semantics tree every window already
publishes for assistive technology and automation: each node's role, name,
value, state, actions and bounds. One renderer turns that tree into semantic
HTML in the shared components' styles, so the page has what a page should have:
real text that the browser lays out, selects and finds, `<input>` fields that
autofill and take passkeys, native scrollers, and buttons and list items that
assistive technology reads. No widget writes HTML of its own; a widget that
publishes good semantics shows well on a page, and one that shows badly is
fixed in its semantics, where a screen reader gains too.

## The composition

`Driver::composition()` (`crates/ui/ui-core/src/app/driver/session.rs`)
walks the laid-out tree from its root. Each widget becomes a `Composed`
(`crates/ui/ui-core/src/composition.rs`): its path (the name semantic actions
use), its rectangle and clip, its layout node's style as CSS, its scroll, whether
its children flow in that style, and its semantics node with the nodes it
publishes under it. A row a list represents in its own nodes (`Widget::
accessibility_represents_child`) carries the list's node for it: its place in
the list, its level and its disclosure. A leaf whose words cannot say what it
shows (a chart, a map, a canvas, an icon-only button too narrow for its name) is
marked for a picture; `Driver::paint_pictures` (`crates/ui/ui-core/src/app/driver/pictures.rs`) paints each such widget
alone and sends it as a PNG, repainted only when what it paints changed. A
widget that publishes a live picture (`Widget::live_picture`, a call's tile)
is marked for its canvas instead, and no still of it is painted.

## The renderer

`ui::composition::page::Page` (`crates/ui/ui-core/src/composition/page.rs`)
decides by role what each node shows:

- **Words**: text, headings, paragraphs, and buttons, links, tabs, menu and
  list items whose name fits them. The role names the element (`h1`…`h6`, `p`,
  `span`, `button`, `a`); a row's title leads and what it says after it is
  dimmer.
- **Field**: an editable text node is an `<input>` or `<textarea>` (a block
  edited in place inside a document), with its value, selection and autofill.
- **Structure**: a container holds its children; a widget that draws its own
  nodes (a list, a timeline, a launchpad's cards) shows each node as the element
  its role names, numbered under the widget. A named group or dialog says its
  name and description above what it holds.
- **Paint**: the widget's picture, with every node in it that acts standing
  over it, where it is drawn, as a button.
- **Well**: a window node naming a place is filled by the page's host with what
  stands for that window — on a workspace's desktop, that window's own page, shown
  where the compositor shows its surface on a device.
- **Call**: a call's tile carries the picture it shows. The browser decodes the
  call's own stream with WebCodecs (`web/voice-video.mjs`) and draws it into the
  tile's canvas at the rate the native call sends it; the tile's face, name and
  speaking ring are elements over it. No still of the tile is painted, so a
  call has one picture path on the page and in a window.

A container whose children flow in its style is a CSS flex or grid box with
that style, so the browser lays it out. A container that places its children
itself has them at their bounds. The page tells the view its size
(`PageAction::Resize`) when it is shown and when the browser window changes, and
the view lays itself out at that size, as a window does, so the bounds match the
page. The shared components' styles (`page.css`, with the theme's tokens from
`page::theme_css`) give each role its look in light and dark.

## Actions

A press on the page is that node's accessibility action. Every element the page
acts on carries `data-p`, a number `Page` keeps for the widget's path (and, for a
node the widget publishes, the node's key under it) from one render to the next;
`data-a` names the action (`activate`, `toggle`, `slider`, `edit`, `value`) and
`data-t` the node's action target. The script
(`crates/core/tilo-server/src/web/page/page.mjs`) sends one `PageAsk::Act` per
click, key or edit (`tilo_server_protocol::page`); the app worker
(`crates/core/agent-session/src/workspace_actions/pages.rs`) performs it on the
widget the number names through `perform_semantic_named`, the same path a
screen reader or an agent takes, and answers with patches: `Page::patch` sends
only the elements that changed, each replacing the element with its number, or,
for an element that came or went (a menu opening over the window, a row added),
putting it in at its place or taking it out alone, so everything else keeps its
scroll and focus. A surface over the window carries the point it appears from
(`Composed::appears_from`), and the page grows it from there with the theme's
appear timing, as `motion::Appear` does natively. An
open page also asks every few seconds what others changed, so their edits reach
it.

A press that travels is a gesture, not a click: on the shell's canvas or a
window's frame the script sends `PageAsk::Pointer` for the press, its move and
its release, in the view's own logical pixels, and the window hands each to its
widgets as a device's pointer event is handed to them. So a window moved or
resized on the page runs the same widget gesture, and the same typed arranging
action, as on a device; a press that does not travel stays the element's own
click.

The page follows the browser's appearance: `PageAsk::Appearance` names the
light or dark look, the window draws again in its theme's tokens and answers
the page whole, and a person who switches their browser's scheme while reading
gets the same page in the other look.

## Tests

`crates/ui/ui-core/src/composition/page.rs`'s own tests, the widgets' and apps'
tests that render their windows as pages, and the server page's flow in
`crates/core/tilo-server/src/web/page/tests.rs` and
`crates/core/tilo-server/src/web/page/page.test.mjs`. The page walks
(`t test page-notes`, `t test page-tour`) drive real pages in a browser.
