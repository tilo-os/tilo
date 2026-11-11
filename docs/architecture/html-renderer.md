# An HTML renderer for tilo-ui on the web

On the web, Tilo draws every app with WebGPU into one canvas. Text selection,
autofill, passkeys, accessibility and phones all suffer from that. The HTML
renderer shows the same frames as real HTML instead: ui-core keeps layout and
widgets, and each frame's scene becomes DOM (boxes, SVG, clips, shadows and
real text), positioned absolutely. It is on main behind
`mountDesktop(el, {app, dom: true})`, off by default.

This document has two parts:
- **[The foundation](#the-foundation)**: what is on main, how it works, what
  it measures, and what comes next.
- **[The spike](#the-spike)**: the time-boxed prototype that chose the
  approach, with its measurements, the problems it found and the options it
  recommended. Its numbers are the spike's, not today's.

**The approach in one line:** keep ui-core's layout and widgets. Turn the
scene each frame already produces into DOM, and give that scene stable
widget keys so the DOM is reconciled by identity. Per-widget DOM
implementations of 580 widget types are not needed to get real HTML, and
they would not make the module smaller.

# The foundation

## What is on main

`mountDesktop(el, {app, dom: true})` shows a mount as HTML. Records, input,
layout and the frame loop are the canvas path's own; the scene goes to the
page as DOM operations instead of to WebGPU. The canvas stays in place,
transparent, as the input surface. Nothing changes for a mount without
`dom`.

1. **Stable element identity.** The vello encoding carries keyed marks
   (`vello_encoding::Mark`): a begin with a key and the origin of what it
   names, and an end. Like glyph-run text, they ride inside the encoding,
   so they survive subtree caches, `Scene::append` and transforms.
   `ui::scene_parts` writes them while a host asks for them
   (`key_widgets`):
   - every widget gets one key: its parent's key, its index and its type,
     or its test id when it has one. A container draws each child through
     the toolkit's draw paths: `draw_child` / `draw_child_at`, which also
     cull a child outside the viewport, or `child.draw_child(…)`
     (`widget::DrawChild`), which keys without culling, for a container
     that culls itself or draws a child where its rect is not. Calling a
     child's `Widget::draw` directly is a clippy error (`disallowed-methods`
     in `clippy.toml` and the crates' own copies), so new code cannot skip
     the key. Tests may draw the widget under test directly;
   - widgets that draw many items themselves key each item by its own id
     (`scene_parts::item_key`): the block editor's blocks and a virtual
     list's rows.

   The page (`web/dom-present.mjs`) gives each key one element, placed at
   the widget's origin, and names everything inside by its kind and place
   among its kind in the innermost widget or clip. Elements are never
   reused by position across widgets: an inserted op renumbers only its
   own kind in its own widget, a moved widget is one position write, and an
   element is reused only for an op of its own kind.
2. **Native scroll containers.** A widget with a scroll region
   (`Widget::primary_vertical_scroll`) reports it as it is drawn, and the
   page turns the clip of its viewport into an `overflow: auto` container.
   The content is placed where it lies in the scrolled content and the
   container is scrolled to the widget's offset. A wheel over it scrolls it
   natively. Its `scroll` event goes back to the widget
   (`dom_scrolled` → `apply_primary_vertical_scroll`), and the next frame
   draws what came into view without moving anything already there. A
   wrapper that only forwards an inner widget's region (a window, a tile,
   an app's root) leaves the region to the innermost widget.
3. **Present from input.** A key, a click or committed text presents its
   frame in a task right after the input's own (one `MessageChannel`
   message), so the keydown and the text it commits share one frame and
   nothing waits for the next animation frame.
4. **One tree for paint and accessibility.** Every painted leaf is
   `aria-hidden`. The inspection tree names the key each widget drew under
   (`paint_key`), and the semantic mirror places that widget's semantic
   element inside the widget's painted element instead of in its own
   layer (`semanticBridge(…, host)`). The page then holds one tree: the
   painted element of a button holds its `<button>`, a field's holds its
   `<textarea>`. Nodes whose widget drew no key stay in the mirror's layer.
5. **Theme.** The theme is published on the HTML layer as custom properties
   (`--tilo-ground`, `--tilo-text`, `--tilo-accent`, `--tilo-radius` and
   more) with `color-scheme`. Light or dark follows the browser's
   `prefers-color-scheme` while the person has chosen neither in their
   settings, and changes with it.

6. **The HTML layer is the hit target** (`web/dom-input.mjs`). The canvas
   lies under the layer, where winit listens, and the page forwards what it
   does not handle itself as the same pointer events, without bubbling:
   - a wheel over a scroll container that can still move moves it in the
     wheel event itself, so the next frame shows it; any other wheel goes
     to the runtime;
   - a touch that lands on a scroll container pans it natively, with the
     browser's momentum and overscroll. The runtime hears the touch begin
     and end, so a tap is still a tap, and a pan the browser takes over
     ends in a cancel, so the runtime never drags what the browser
     scrolls. Elsewhere a touch is the runtime's (`touch-action: none`);
   - a press is the runtime's, as on the canvas: its own focus change and
     selection are prevented, or the page's focus would leave the runtime's
     editor for the page and the semantic mirror would take it;
   - `__tiloDom.selectText(true)` hands presses to the page for native
     selection; while the person makes or holds one, neither the host nor
     the semantic mirror moves the focus, and the copy shortcut is the
     browser's;
   - the runtime's cursor, set on the canvas, shows on the layer.
7. **Images and gradients.** An image is handed to the page once, as its
   pixels under an id, and shown as a `<canvas>` placed by the image's own
   matrix and clipped to what the scene draws it into; both sides forget an
   image no frame showed for a while. A linear, radial (one centre) or sweep
   gradient filling a box becomes the CSS gradient that paints the same
   pixels, laid out against the box. A gradient on a path, a stroke or text
   still shows its first stop.
8. **Text as flows.** The runs a widget draws one after another, each
   further along its line or on a line below, are one element: a block per
   line, as tall as the step to the next, holding its runs side by side.
   Each run stands where the toolkit set it: a line's first run is indented
   to its x, and a run followed on its line is exactly as wide as the step
   to the next run, so the browser's own advances never move what follows.
   Line breaks stay the toolkit's. A selection or find-in-page then reads a
   paragraph as one text, with a line break between its lines. A justified
   line is one run whose spaces the toolkit stretched; the page sets spaces
   at the font's own width, so such a run goes over as one run per word.
9. **Native selection holds.** A frame moves the fewest elements it can
   (the longest run already in order stays), and never the one holding the
   page's selection; moving a node takes the selection out of what it
   holds. Root widgets are numbered per scene and per type, so a widget
   drawn at the root of another scene (a measuring pass) no longer
   renumbers the frame's own: on a press that blurred the editor, every
   root key changed and the note's panel was rebuilt under the selection.
10. **Canvas islands.** A widget that says it is a picture
    (`scene_parts::picture`: charts, plots, maps) or that makes
    `ISLAND_DRAWS` (400) draws of its own is shown as one image of what it
    drew, cut to the viewport, with its text still text over it. The
    encoder rasterizes it with the CPU rasterizer the canvas path already
    has, hashes what it drew, and hands the page a new image only when that
    changed, naming the one it replaces. A widget that scrolls natively or
    holds a field the page shows never becomes an image unasked.
11. **Real `<input>` for single-line fields.** A text field's semantics
    carry what it asks to be filled with (`AccessState::autofill`: username,
    email, current or new password, one-time code, name, telephone), which a
    `TextInput` takes from its kind or `with_autofill`. On a page shown as
    HTML a single-line field leaves its text, caret, selection and
    decorations to the page (`scene_parts::native_field`): it still draws
    its chrome and placeholder, and reports where its line starts, its
    baseline, the face, size and colour (`meta.fields`). The semantic
    mirror makes the field an `<input>` (`type="password"` for a secret,
    `autocomplete` from the purpose: `username webauthn` on a username, so
    a page's conditional passkey request is offered there), the presenter
    shows it in the field's painted element with padding that puts its line
    where the toolkit's would stand, and the layer lets presses on it
    through: the browser's caret, selection, IME, autofill and passkeys are
    the field's. What the person types reaches the widget through the
    mirror's value and selection actions. A password input keeps what was
    typed: the tree never holds a secret, so it cannot empty it either. A
    field moved to another element while the person types in it keeps its
    focus and caret; the mirror is told the blur and focus are the page's
    own.

Everything that only an HTML mount runs is reached through `DOM_CODE`
(`ui::lazy_code!`): drawing the frame, the toolkit's keying code (the
`ui::scene_parts::Keyer` it installs; the draw paths carry only the check
whether one is installed), the encoder (`tilo_cpu_raster::dom`) and the
presenter that hands frames to the page. The browser desktop splits it into
its own module (`dom_presenter`) and fetches it the first time a page asks
for `dom: true`.

## Measurements

Three runs of `tools/web/html-renderer/run-measure.sh` on the x86 builder,
the last on the code as it landed: headless Chrome for Testing, no GPU, so
WebGPU runs on SwiftShader (its latency is pessimistic, as in the spike).
The baseline is the plain-DOM page (`baseline.html`, a contenteditable
document with a sidebar) in the same run. 42 keystrokes, 60 wheel steps (30
at 390 px), 60 ms apart. Latency is from the input event to the first
animation frame after the page changed; for a native scroll the change is
the scroll itself. Raw data of the latest run, with every change below:
[`html-renderer/results.json`](html-renderer/results.json); the spike's is
[`html-renderer/spike-results.json`](html-renderer/spike-results.json).

| | HTML, last run | HTML, the three runs | HTML (spike) | WebGPU canvas | Plain DOM, the three runs |
|---|---|---|---|---|---|
| typing p50, 1280 px | 23.1 ms | 13.8 – 23.1 ms | 24.6 ms | 266 – 399 ms | 1.9 – 3.2 ms |
| typing p90, 1280 px | 36.9 ms | 23.2 – 36.9 ms | 39.8 ms | 496 – 882 ms | 3.2 – 3.8 ms |
| typing p50 / p90, 390 px | 15.7 / 29.7 ms | p50 13.0 – 15.7 ms | 26.4 / 47.9 ms | — | — |
| wheel scroll p50, 1280 px | 2.8 ms | 2.6 – 4.9 ms | 48.9 ms | 63 – 65 ms | 17.0 – 20.2 ms |
| wheel scroll p90, 1280 px | 58.1 ms | 38.8 – 58.1 ms | 82.8 ms | 617 – 659 ms | 23.3 – 25.5 ms |
| wheel scroll p50, 390 px | 21.2 ms | 20.4 – 21.2 ms | 60.8 ms | — | — |
| property writes per keystroke frame, p50 / max | 2 / 63 | the same | up to 650 | — | — |
| new elements per keystroke frame, p50 / max | 0 / 7 | the same | — | — | — |
| presenter time per keystroke frame, p50 / max | 0.6 / 5.3 ms | p50 0.4 – 0.6 ms | 0.3 – 1.3 ms | — | — |
| memory, all Chrome processes, 1280 px | 932 MB (GPU process 107) | 925 – 932 MB | 939 MB | 1246 MB (GPU process 495) | 513 MB |

Against the targets:
- **Typing within one frame of plain DOM:** not yet reliably. The median was
  within one 16.7 ms frame in two runs (10.6 and 14.5 ms behind) and 20.7
  ms behind in the third; p90 is one to two frames behind. The page's own
  work per key is small (2 property writes, no new element, under a
  millisecond in the presenter); what is left is layout in wasm, its
  variance on a shared builder, and the browser's style and paint of the
  changed line. The next step there is measuring the remaining time from a
  trace on a quiet machine, not from headless timings.
- **Scrolling close to plain DOM:** met where the scroll is native. At 1280
  px the median is below the baseline's: the browser moves the container,
  and the frame that follows only adds what came into view. The p90 is the
  steps that reach the top of the note: a wheel the container cannot take
  goes to ui-core and waits for a frame. At 390 px the median matches plain
  DOM.
- **No visual difference from WebGPU:** the same note before and after the
  same keystrokes, HTML against the canvas, compared pixel by pixel
  (`notes-diff-*.png`): 1.1 – 1.2 % of pixels at 1280 px (light, typed,
  dark) and 4.5 % at 390 px differ by more than 48 of 255 in a channel,
  mean difference 1.7 and 5.9, the same in every run. Every difference is on
  glyph edges: the browser rasterizes and antialiases text itself, where
  vello does on the canvas. Line breaks, positions, icons, caret and
  scrollbar are the same. The phone figure is higher only because its text
  is set at three device pixels per point, so more of each glyph is edge.
- **Accessibility:** no painted text is exposed to assistive technology (0
  of 47 text elements). With every widget keyed, 523 semantic elements live
  inside their widget's painted element and 7 are left in the mirror's
  layer (before: 14 and 517).
- **With every widget keyed** (one run on the same builder): typing p50 /
  p90 19.2 / 32.9 ms at 1280 px and 12.0 / 43.4 ms at 390 px; wheel p50 /
  p90 16.6 / 59.5 ms at 1280 px and 5.4 / 40.6 ms at 390 px (before: 21.2
  ms p50). The 1280 px frame holds 241 elements (before: 118), since every
  widget has one; a scroll frame writes 112 properties and creates 19
  elements at the median (before: 103 and 13). The 1280 px wheel median is
  one frame above the previous runs' 2.6 – 4.9 ms with the same work per
  frame; it needs a run on a quiet machine. Pixel parity with the canvas
  is unchanged (1.1 – 1.2 % at 1280 px, 4.5 % at 390 px).
- **Light and dark** follow the emulated `prefers-color-scheme` in the
  HTML runs with no Tilo setting given.
- **With the HTML layer as the hit target** (two runs, each with the canvas
  and plain DOM in the same run, the builder otherwise quiet):

  | | HTML | WebGPU canvas | Plain DOM |
  |---|---|---|---|
  | typing p50 / p90, 1280 px | 14.7 – 15.8 / 29.7 – 39.5 ms | 253 – 264 / 489 – 535 ms | 2.0 – 2.2 / 2.8 – 4.3 ms |
  | wheel p50 / p90, 1280 px | 30.7 – 36.6 / 54.5 – 72.0 ms | 64.8 – 69.1 / 620 – 631 ms | 20.8 – 21.1 / 26.8 – 29.2 ms |
  | typing p50, 390 px | 17.5 – 19.6 ms | — | — |
  | wheel p50, 390 px | 22.1 – 27.2 ms | — | — |
  | touch pan of the note, 390 px | 280 px for a 260 px drag, native | — | — |

  The wheel median is about one frame above plain DOM and above the run
  with the canvas as the hit target (16.6 ms); the work per scroll frame
  is the same (99 writes, 14 new elements, 1.3 ms in the presenter).
  Pixel parity is unchanged (1.2 % at 1280 px, 4.5 % at 390 px). A tap
  opens the note as on the canvas: an earlier difference at 390 px was a
  passing "Saved" notice caught in one screenshot and not the other.
- **With paragraphs as flows** (one run with the canvas and plain DOM, one
  of HTML alone, today's builder): a drag over the note after typing
  selects from where it was pressed to where it was let go, across lines
  and paragraphs ("Paragraph 25: … keep every thought close at hand, and
  the page keeps scrolling. / Paragraph 26: the quick brown fox …"); before,
  it came back empty. Typing p50 / p90 17.0 – 30.3 / 24.6 – 51.3 ms at 1280
  px and 14.9 / 25.6 ms at 390 px; wheel p50 21.0 – 22.0 ms (plain DOM 16.4
  in the same run). 524 semantic elements live inside their widget's
  painted element, 6 in the mirror's layer. Pixel parity is unchanged at
  1280 px (1.2 %); at 390 px 5.8 %, from the passing "Saved" notice caught
  in the HTML screenshot and not the canvas one. The median keystroke
  still writes 2 properties and creates nothing; the largest frame of the
  run wrote 1,510 and created 287.
- **The last run, with every change in this stage** (HTML, canvas and
  plain DOM in one run; raw data in
  [`html-renderer/results.json`](html-renderer/results.json)): typing p50 /
  p90 19.6 / 32.8 ms at 1280 px and 16.1 / 30.8 ms at 390 px (plain DOM
  2.8 / 3.6, the canvas 395 / 886 on SwiftShader); wheel p50 22.0 ms
  (plain DOM 17.1). A keystroke frame writes 2 properties at the median
  and at most 63, and creates at most 7 elements: since root keys hold,
  no frame rebuilds the mount. The drag after typing selects what it
  covers. Pixel parity 1.2 % at 1280 px (light, typed, dark) and 4.8 % at
  390 px. A touch drag of 260 px pans the note 390 px natively. Memory, all
  Chrome processes: 744 MB as HTML, 1105 MB on the canvas, 514 MB for the
  plain page.
- **Every app of the desktop bundle** (`parity.mjs`, all 39, 1280 px,
  light, each mounted alone and left to settle, HTML and canvas in the same
  run): the median app differs from the canvas in 0.16 % of its pixels, 37
  apps in under 0.7 %; the diffs looked at (Maps, Notes, Passwords, Themes)
  are glyph edges. Two stand out. Themes
  (1.6 %): its theme previews are small text drawn as part of a picture of
  a window. ebook (4.8 %): see the open items below. No app logs an error
  as HTML. 1,074 semantic elements live inside their widget's painted
  element and 371 in the mirror's layer (Model providers alone has 111 of
  them); no painted text is exposed to assistive technology. Five apps show
  a single-line field on their first screen, all as real inputs. Typing
  into a first field: 6.7 – 23.5 ms p50 (plain DOM 2.1 in the same run).
  Wheel steps over the middle of the app: 19.7 ms p50 over all apps (plain
  DOM 18.2); the slowest, 25 – 31 ms, are apps whose middle does not scroll
  natively, so the wheel goes to the runtime and waits for its frame.
- **The wheel through the layer** (step 3's 30.7 – 36.6 ms p50, against
  16.6 ms before it): two ways of scrolling, each run twice against plain
  DOM on today's builder. With the layer setting the scroll container's offset in
  the wheel event itself, as main does, the median is 21.0 – 22.8 ms (plain
  DOM 16.4 – 21.3 ms in the same runs, one outlier at 8.4). Letting the
  browser scroll the container itself, through a passive listener, gives
  37.3 – 38.1 ms: Chrome animates a wheel scroll and its first step lands a
  frame later. Step 3's numbers match the browser's own path, not the code
  that landed; on main the wheel is at plain DOM's level, and no change was
  needed.
- **Size with `dom` off** (fields, islands and the keys' fixes): the
  desktop bundle's start module is 21,649,224 bytes against 21,639,469 for
  the main it is based on, built on the same builder: 9.8 KB, 0.05 %,
  larger: the text field's call that hands its line to the page, the
  autofill purpose in its semantics, and the picture calls of charts, plots
  and maps. The HTML module (`dom_presenter`) grew from 53 KB to 91 KB with
  the island rasterizer.
- **Size with `dom` off:** keying every widget leaves the desktop bundle's
  start module (as the `sdk-web-size` check builds it,
  `tools/web/html-renderer/size.sh`) at 20,295,058 bytes against 20,281,659
  for the main it is based on, built on the same builder: 13 KB, 0.07 %,
  larger, with no module among those that grew most. A first version that
  made every child a trait object grew it by 35 – 67 KB: a type that was
  never a trait object got a vtable, which brings all of its `Widget`
  methods in. A child held by value is now drawn through one generic,
  never-inlined method per type. The keying code, the encoder, the
  presenter and the HTML frame path stay in their own module
  (`dom_presenter`), fetched only by a page that asks for `dom: true`.

## Is `dom: true` ready to be the web default?

Not yet. What is ready: every app of the desktop bundle draws as HTML
without an error and within 1 % of the canvas's pixels but for two; text
is real, selectable and in one flow per paragraph; scroll containers are
native, with the browser's wheel and touch; sign-in fields are real inputs
that autofill and offer passkeys on; the semantic tree is one with the
painted one for most widgets; a widget key no longer moves when the
container that draws it leaves a child out; the start module did not grow
beyond noise.
What blocks it:

- **Typing in painted text** (the editors: Notes, the word processor) is
  still behind a plain page's 2 – 3 ms. Half of a keystroke was waiting, not
  working: the frame the input asked for was drawn in a task of its own, and
  a rendering update often came between the two. The frame is now drawn
  inside the input's own task, and a frame nothing changed is skipped. What
  is left is still two frames per key (the frame schedule asks for an
  animation frame after the first) and a layout pass of about 4 ms in wasm,
  plus the browser's style, layout and paint of the changed line; the next step is a CPU trace per key on a
  quiet machine, not another headless timing run. The measured split per
  keystroke (`typingBreakdown` in `results.json`: input → frame handed over,
  and present → first paint) is what that trace starts from, and the phone
  width improved with the change from 23.1 / 44.4 ms to 18.2 / 29.6 ms
  (p50 / p90) in runs whose plain-DOM baseline stayed sane.
- **Not every app is measured past its first screen.** The parity run
  mounts each app and types into its first field; menus, dialogs, drags,
  the canvas app's drawing and the video editor's timeline are not driven
  yet, and ebook's pages differ from the canvas for a reason not found.
- **Phones.** Only Notes is measured at 390 px. Pictures are rasterized at
  one pixel per CSS pixel, so they are soft on a phone's screen.
- **Real hardware.** Every number here is headless Chrome without a GPU;
  Safari and Firefox are not run at all.

## Reproducing

```sh
# on a build machine, from a worktree of main; fetch the results back after
tools/web/html-renderer/all.sh   # Notes: --select, --wheel, --phone or --quick run one part
tools/web/html-renderer/all-desktop.sh   # the sign-in fields, then every app of the desktop bundle
tools/web/html-renderer/size.sh   # the desktop bundle's start module, with `dom` off
```

The bundle lands in `$CARGO_TARGET_DIR/htmlrenderer/bundle`; open
`app.html?render=dom` or `?render=gpu` (`&app=` names another app of the
bundle, `&theme=light|dark` sets the person's setting). Results, screenshots and the HTML-against-WebGPU diff
images land in `$CARGO_TARGET_DIR/htmlrenderer/measure`, every app's in
`$CARGO_TARGET_DIR/htmlrenderer/parity`; they are not in the repository,
which takes no binary files.


# The spike

## What the prototype does

The prototype is on branch `spike/html-renderer`. It is a presentation mode
of the existing web runtime: `mountDesktop(el, {app, dom: true})`. Records,
input, the semantics mirror and the frame loop are unchanged. Only the last
step differs: the scene goes to the page as DOM operations instead of to
WebGPU.

1. **Glyph runs carry their characters.** `vello_encoding::GlyphRun` gains
   `text: Option<Arc<str>>` (vendored vello fork), set with
   `DrawGlyphs::text`. The toolkit's text renderer
   (`ui-core/src/text_render.rs`) is the only place that draws glyphs, in
   two spots (plain and rich text). It fills the field when a host calls
   `ui::text_render::present_text(true)`. The text travels inside the scene,
   so it survives scene caching, `Scene::append`, transforms and retained
   parts. The GPU never reads it.
2. **The scene becomes DOM operations** (`tilo-cpu-raster/src/dom.rs`). The
   CPU rasterizer's scene walker already decodes vello encodings, and the
   new module lists each draw as the plainest element that shows it:
   - axis-aligned rectangles and rounded rectangles become boxes with a
     background, border and radius;
   - other paths and icon glyphs become SVG paths;
   - glyph runs with characters become text;
   - rectangular clips become `overflow: hidden` boxes, other clips
     `clip-path`;
   - blurred rectangles become `box-shadow`;
   - images become placeholders in the spike.

   Text is set in the very font files the toolkit shaped it with. Each face
   is registered once as a `FontFace` from its bytes, with its variation
   axes (`font-variation-settings`) and the font's own ascent and descent,
   so the browser's baseline lands on the toolkit's.
3. **The page builds and reconciles the elements**
   (`tilo-browser-runtime/web/dom-present.mjs`). Elements are reused frame
   to frame, and only changed properties are written. An unchanged frame
   writes nothing.
4. **The canvas stays as the input surface**, transparent above the HTML
   layer, so winit's pointer and keyboard paths are untouched.
   `__tiloDom.selectText(true)` lets the pointer through to the HTML layer
   for native selection.

Notes is the app rendered end to end:
- Its data is live: a note imported through the app's own `import-markdown`
  action and opened from the sidebar with a click.
- Typing re-wraps the paragraph and updates "edited today at …".
- Scrolling, keyboard focus, light and dark (the person's `appearance.mode`)
  and the 390 px phone layout all work.

`tools/web/html-renderer/` builds the bundle and runs the measurements in
headless Chrome on the builder. The scripts are `build-notes-web.sh`,
`run-measure.sh` and `measure.mjs`; `attribute.py` attributes code size to
crates.

`run-measure.sh` writes screenshots beside `results.json`; they are not
in the repository, which takes no binary files:

- `notes-dom-1280.png`, `notes-dom-390.png`: Notes as HTML, light;
- `notes-dom-1280-dark.png`, `notes-dom-390-dark.png`: the same, dark;
- `notes-dom-1280-typed.png` and `notes-gpu-1280-typed.png`: HTML and
  WebGPU after the same keystrokes into paragraph 2;
- `notes-dom-390-typed.png`: the phone layout after typing;
- `notes-dom-1280-selected.png`: the browser's own selection.

The HTML and WebGPU frames are visually the same: the same text positions,
line breaks, icons, caret and scrollbar. The 1280 px light frame has 47 text
elements and 161 draws, or 118 DOM elements in all.

## Measurements

These numbers come from one release bundle (`wasm-release`, wasm-opt), so
the two renderers are compared on the same module. Both run in headless
Chrome for Testing on the x86 builder, with no GPU. WebGPU there runs on
SwiftShader, Vulkan emulated on the CPU, so the **WebGPU latency and
GPU-process memory are pessimistic**. On a real GPU they would be much
lower. The plain-DOM baseline is `tools/web/html-renderer/baseline.html`: a
contenteditable document of 200 paragraphs with a sidebar.

Latency is measured from the input event's timestamp to the first animation
frame that starts after the page changed. There were 42 keystrokes and 60
wheel steps (30 at 390 px), one at a time, 60 ms apart. Raw data:
[`html-renderer/results.json`](html-renderer/spike-results.json).

| | HTML (DOM mode) | WebGPU canvas | Plain DOM |
|---|---|---|---|
| wasm, raw / brotli -q 11 | 47.96 MB / 11.68 MB | same module | — |
| JS, raw / brotli | 555 KB / 116 KB | same | 1.4 KB |
| startup fonts, raw / brotli | 406 KB / 181 KB | same | — |
| first frame after navigation, 1280 / 390 px | 0.95 s / 1.33 s | 1.01 s / 0.98 s | — |
| typing, p50 / p90, 1280 px | **24.6 / 39.8 ms** | 290 / 510 ms (SwiftShader) | 2.5 / 3.8 ms |
| typing, p50 / p90, 390 px | 26.4 / 47.9 ms | — | — |
| wheel scroll, p50 / p90, 1280 px | 48.9 / 82.8 ms | 64 / 611 ms (SwiftShader) | 8.2 / 27.7 ms |
| wheel scroll, p50 / p90, 390 px | 60.8 / 116.5 ms | — | — |
| memory, all Chrome processes (PSS), 1280 px | 939 MB (GPU process 108) | 1288 MB (GPU process 522) | 515 MB |
| memory, 390 px, before interaction | 918 MB | 1009 MB (GPU process 450) | — |
| per frame in wasm: layout / DOM encode | 0–3 ms / 0.3–0.5 ms | layout same; raster on GPU | — |
| per frame in JS: reconcile | 0.3–1.3 ms, 0–650 property writes | — | — |

How to read the table:

- **Size is the same module either way.** Turning WebGPU off does not
  shrink the bundle by itself. The [attribution](html-renderer/attribution.json)
  of the runtime module's 40.0 MB of function bodies (an unstripped build,
  by defining crate) shows that a DOM-only page could drop very little:
  - **GPU renderer, about 0.8 MB (2%):** wgpu 52 KB, naga 542 KB,
    vello 151 KB, vello_shaders 44 KB.
  - **Text stack, about 1.6 MB (4%):** harfrust 382 KB, skrifa 500 KB,
    read_fonts 533 KB, cosmic_text 102 KB and segmentation, and that only
    if the browser also did text layout.
  - **Layout:** taffy 258 KB.

  The module is dominated by:
  - **the toolkit:** tilo_ui 5.7 MB;
  - **generics:** core and alloc 7 MB;
  - **serialization:** serde 3.2 MB;
  - **sync, crypto and networking:** automerge, peer-state, moq, iroh,
    openmls, rustls, about 5 MB;
  - **search:** tantivy, about 1.5 MB;
  - **the desktop crates:** about 2 MB.

  The under-5 MB goal is reached by splitting and lazy loading those, not by
  the renderer choice.
- **First frame** is about the same, roughly 1 s, and noisy (±0.4 s between
  runs). It is dominated by compiling and starting the module. The WebGPU
  path already shows its first frames from the CPU rasterizer while
  pipelines compile, so DOM mode removes no visible wait here. On a phone
  without WebGPU, DOM mode is the difference between a page and an error:
  `mountDesktop` refuses without WebGPU today.
- **Typing latency** in DOM mode is about 25 ms against 2.5 ms for plain
  HTML. The wasm and JS work per keystroke is about 4 ms: layout up to
  3 ms, encode 0.4 ms, reconcile up to 1.3 ms. The rest is scheduling.
  Input goes to winit, the frame waits for the next animation frame, and
  the change shows in the frame after that. A plain page changes the DOM in
  the input event's own task. Presenting synchronously from the input
  handler would remove most of the 20 ms.
- **Scrolling** in DOM mode is wheel → ui-core scroll offset → a whole new
  frame, and with position-keyed reuse up to 650 property writes per step.
  A native scroll container costs the browser nothing per step, so this
  path is slower than plain HTML by design.
- **Memory:** the DOM page saves the GPU process's working set: 108 MB
  against 450–520 MB on SwiftShader. A real GPU's figure would be lower but
  not zero. The renderer process holds a little more for the DOM: about
  60 MB more at 1280 px.

## What worked

- **Real text from every app, unchanged.** Every app draws through the one
  scene. With characters on glyph runs, everything the toolkit shows
  becomes real text, with no app or widget change:
  - title, metadata, headings, paragraphs and list bullets;
  - sidebar, outline and toolbar icons;
  - caret, focus rings, selection highlight, scrollbar.

  A repository-wide count shows why that matters. There are 408 widget
  types in ui-core and 173 in apps; 164 app `draw` methods across about 50
  apps; and 1,541 direct paint calls (1,024 toolkit, 517 apps). Hand
  written DOM for each is the expensive alternative.
- **Layout parity.** The toolkit lays out (taffy and cosmic-text) and the
  DOM is absolutely positioned. The browser sets each line in the same font
  file, so its glyph advances match the toolkit's, and line breaks are the
  toolkit's. The HTML and WebGPU screenshots match.
- **The phone layout and light and dark** come for free,
  because they are the app's own layout and theme.
- **Live data, typing, focus and scrolling** go through the unchanged
  input, record and frame paths.
- **Native selection over the text.** The browser's own drag selection
  works on the HTML text (`notes-dom-1280-selected.png`).
  That drag selected sidebar text because an empty clip box lay over the
  note. Clips and boxes no longer take the pointer, but see the focus
  problem below.
- **A small change set.** The prototype adds about 1,000 lines:
  - glyph-run text in vello: 14;
  - text renderer: 57;
  - DOM encoder and walker: 570;
  - runtime presentation path: 160;
  - page presenter and host hook: 200.

## The hard problems, with a recommended option each

### 1. Layout ownership: ui-core layout or CSS layout

- **Options:**
  - (a) ui-core lays out and the DOM is absolutely positioned (the
    prototype);
  - (b) translate taffy styles to CSS flexbox and grid and let the browser
    lay out;
  - (c) per-widget DOM with CSS layout.
- **Recommended: (a).** It is the only option that keeps one layout for
  native and web: the same breakpoints, the same `MANUAL_LAYOUT`
  widgets (14 manual policies), the same pixels. It needs no change in 580
  widget types. (b) breaks on every `measure`, manual and
  `adjust_layout_style` widget. (c) is the per-widget rewrite. The cost of
  (a) is that resize and reflow run in wasm. At 0–3 ms per frame for Notes
  that is acceptable.

### 2. Text measurement and text as a document

- **Problem:** the toolkit breaks lines, and the browser sets each line.
  Widths match because the font files are the same. A mismatch could only
  shift text within a line, never re-wrap it, because each line is
  positioned on its own. Per-line spans cost in two ways:
  - a selection or a screen reader sees lines, not paragraphs;
  - find-in-page works on the text but cannot match across a line break.
- **Recommended:** keep per-line placement, but group the lines of one
  paragraph into one element. Use the toolkit's breaks as forced breaks
  (`<br>`, `white-space: pre`). The scene needs to say which runs belong to
  one paragraph, which is the same keying as problem 4. Keep
  `advance_to_last`, which the encoder already emits, to correct width
  drift with `letter-spacing` if a face ever disagrees.

### 3. Custom drawing

- **Problem:**
  - Most custom drawing (charts, the canvas app, maps, video, the code
    editor's gutter) is paths, images and glyphs. Paths already come out
    as SVG.
  - Heavy scenes (thousands of paths, raster images, blend layers) would
    make thousands of elements.
  - Images, gradients and luminance masks are placeholders or
    approximations in the spike: a gradient shows its first stop.
- **Recommended:** keep scene-to-DOM as the default, and add a *canvas
  island*. A widget that declares itself a picture, or a subtree whose op
  count crosses a threshold, is rasterized into one `<canvas>` element at
  its rect. The CPU rasterizer that already exists (`tilo-cpu-raster`,
  135 KB of vello_cpu) can do it, or WebGPU when present. Images become
  `<img>` or `ImageBitmap` canvases fed from the image store. Linear and
  radial gradients map to CSS gradients.

### 4. Element identity: reconciling by position

- **Problem:** the prototype reuses elements by position in the op list.
  - **A real bug:** a box that had been the scrollbar came back as the main
    panel's clip and kept its grey background, painting over the note.
    That is fixed by keying on the op kind, but it shows the fragility.
  - One inserted op shifts every later element.
  - A scroll step rewrites about 600 properties.
  - CSS transitions and transforms cannot follow a widget.
- **Recommended:** mark widget boundaries in the scene with stable keys.
  `ui-core/src/scene_parts.rs` already does this for damage tracking:
  keyed parts with bounds, carried through scene appends. Emit a part per
  retained widget (its test id or path), and give the DOM one element per
  part. The DOM is then reconciled by key. A moved widget is one transform
  write. Scrolling can move a container instead of rewriting its children.
  The accessibility element and the painted element can be the same node
  (problem 6).

### 5. Scrolling and input latency

- **Problem:**
  - Scrolling is ui-core-driven, so each wheel step is a new frame:
    49–61 ms p50 against 8 ms native.
  - Phones expect native momentum and overscroll.
  - Typing waits one to two animation frames.
- **Recommended:**
  - Map scroll regions (`primary_vertical_scroll`, `child_clip` in the
    inspection tree) to native `overflow: auto` containers. The keyed
    content is laid out once at its full extent, or virtualized by
    ui-core's existing `for_each_child_in_clip`. Feed the browser's scroll
    position back to ui-core as the scroll offset.
  - Present synchronously from input handlers in DOM mode, with no wait
    for the next animation frame.

### 6. Accessibility tree mapping

- **What exists:** the web runtime already projects the AccessKit tree as
  DOM (`web/semantics.mjs`): 532 elements on the Notes page with real roles
  (button, tree, treeitem, textbox, heading, slider, document), placed over
  each control, with editable text as textareas.
- **Problem:** in DOM mode that mirror and the painted text both exist, so
  assistive technology could read text twice. The prototype does not mark
  the paint layer `aria-hidden` yet.
- **Recommended:**
  - First, mark the paint layer `aria-hidden`; that is today's behaviour
    with the canvas.
  - Then, with keyed parts (problem 4), merge the two: the semantic element
    becomes the container of its painted content. A button's label is then
    its own text node, and a heading is an `<h2>` around its text.

### 7. Inputs, autofill and passkeys

- **Problem:**
  - Text fields are drawn by ui-core and fed by one hidden IME textarea.
    The browser cannot autofill a field it cannot see as an `<input>`, and
    it offers passkeys only on an input with
    `autocomplete="username webauthn"`.
  - **Likely focus conflict:** the host keeps keyboard focus in its hidden
    editor and takes it back on blur. A native selection made after typing
    in a note came back empty. In an earlier run, where the click had not
    focused the editor, the same drag selected real text. The likely cause
    is that the host takes focus back; that was not verified.
- **Recommended:**
  - For single-line fields and the identity flows, render the semantic
    mirror's editable element as the visible field (`<input>`) at the
    widget's rect, with the toolkit's font and colours. Add an "autofill
    purpose" to text-input semantics (username, password, one-time code,
    email) and stop drawing that field's text in the scene. The browser
    then owns caret, selection, autofill and passkey UI, and the value
    flows through the existing `value` and `selection` semantic actions.
  - Keep the rich editors (Notes, the word processor) on ui-core's editing
    model.
  - Make the host stop reclaiming focus while the person has a page
    selection. Copy should come from the DOM selection.

### 8. Animation and transitions

- **Problem:** ui-core animates by laying out and drawing every frame. In
  DOM mode each animation frame is encode plus reconcile: 1–5 ms for this
  note, which is fine for small motion. Large motion (window moves, drawer
  slides) is many writes per frame.
- **Recommended:** once elements are keyed (problem 4), a part whose
  content did not change but moved becomes a `transform` write. That is
  cheap and composited. Leave timing in ui-core, so motion matches native
  and respects reduced motion. Do not hand animations to CSS transitions.

### 9. Theming tokens and CSS variables

- **Problem:** the scene carries resolved colours, so theme switches
  already work: light and dark come from `appearance.mode` and re-render.
  The page itself does not follow `prefers-color-scheme`. Browser-drawn
  parts (form controls, scrollbars, autofill popups) need `color-scheme`.
- **Recommended:**
  - Default `appearance.mode` to automatic from `prefers-color-scheme` on
    the web.
  - Publish the theme's tokens as CSS custom properties on the mount root
    (surfaces, text, accent, radii, the UI face), together with
    `color-scheme`, for the elements of problem 7 and for page-native
    chrome.
  - Painted content keeps resolved colours; converting every scene colour
    to a variable gains nothing.

### 10. Apps that mix both

- **Recommended:** both renderers present the same scene, so mixing is per
  element, not per app. A subtree goes HTML by default, or becomes a canvas
  island (problem 3) when it is a picture, a video or too heavy. A whole
  mount can fall back to WebGPU when present, for heavy apps (canvas,
  video editor, maps). Make WebGPU, vello's GPU renderer and naga a lazily
  loaded split module (0.8 MB) that only those apps fetch.

## Estimate for a production renderer for all apps

Assuming one engineer who knows ui-core, working from this prototype:

| work | weeks |
|---|---|
| Keyed scene parts per retained widget; DOM reconciled by key; paragraph grouping | 3 |
| Native scroll containers mapped to ui-core scroll regions; synchronous present on input | 2–3 |
| Images, gradients, blend layers, masks; canvas islands through the CPU rasterizer or WebGPU | 2 |
| Accessibility merge (semantic element as the container of its paint); `aria-hidden` until then | 2 |
| Real `<input>` for single-line fields with autofill purposes; passkeys; focus and selection hand-off | 3 |
| Theme tokens as CSS variables, `color-scheme`, automatic light and dark | 0.5 |
| WebGPU, vello and naga as a lazy split module; DOM as the default web path | 1 |
| Visual parity harness (HTML vs WebGPU screenshots for every app scenario) and fixing what it finds across about 70 apps | 3–4 |
| **Total** | **about 16–19 weeks** |

A per-widget DOM implementation instead (every ui-core widget and every
app's custom `draw` re-expressed as DOM, CSS layout) is the larger job:
580 widget types and 164 app `draw` methods. Estimated at 9–12 months. It
would still need canvas islands, and it splits native and web into two
layouts.

## Reproducing

```sh
# on a build machine, from a worktree of branch spike/html-renderer; fetch the
# results back after
tools/web/html-renderer/all.sh     # build + measure
tools/web/html-renderer/attribute.sh
```

The bundle lands in `$CARGO_TARGET_DIR/htmlrenderer/bundle`. Open
`notes.html?render=dom` or `?render=gpu`, with `&theme=light|dark`.
