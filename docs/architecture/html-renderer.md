# An HTML renderer for tilo-ui on the web

On the web, Tilo draws every app with WebGPU into one canvas. Text selection,
autofill, passkeys, accessibility and phones all suffer from that. This
document is the result of a time-boxed spike that asked what it takes to
show tilo-ui apps as real HTML instead. It answers with a working prototype,
measurements against the WebGPU path, the problems that need a decision,
and an estimate.

**Recommendation in one line:** keep ui-core's layout and widgets. Turn the
scene each frame already produces into DOM, and give that scene stable
widget keys so the DOM can be reconciled by identity. Per-widget DOM
implementations of 580 widget types are not needed to get real HTML, and
they would not make the module smaller.

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

`tools/web/html-spike/` builds the bundle and runs the measurements in
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
lower. The plain-DOM baseline is `tools/web/html-spike/baseline.html`: a
contenteditable document of 200 paragraphs with a sidebar.

Latency is measured from the input event's timestamp to the first animation
frame that starts after the page changed. There were 42 keystrokes and 60
wheel steps (30 at 390 px), one at a time, 60 ms apart. Raw data:
[`html-renderer/results.json`](html-renderer/results.json).

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
# on the builder, from a worktree of this branch
rcargo --fetch htmlspike/measure exec tools/web/html-spike/all.sh   # build + measure
rcargo --fetch htmlspike/attribution exec tools/web/html-spike/attribute.sh
```

The bundle lands in `$CARGO_TARGET_DIR/htmlspike/bundle`. Open
`notes.html?render=dom` or `?render=gpu`, with `&theme=light|dark`.
