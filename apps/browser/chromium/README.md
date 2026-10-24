# Native Chromium host

A CEF offscreen browser process for the Rust browser engine. CEF is downloaded
as a pinned, digest-verified binary SDK by `scripts/browser/build-host.py`;
ordinary Cargo builds do not build Chromium.

## Build

From the repository root:

```sh
python3 scripts/browser/build-host.py
```

Alternatively configure CMake with `CEF_ROOT` pointing to the extracted pinned
SDK, `TILO_BLOCKER_LIBRARY` pointing to the Rust blocker static library, and
`TILO_OUTPUT_DIR` to the desired runtime directory. C++20 and CMake 3.21+
are required. macOS uses the Apple command line tools; Linux needs a supported
CEF distribution and the system runtime dependencies listed in its README.

The default macOS executable is:

```
target/chromium/Browser Engine.app/Contents/MacOS/tilo-chromium-host
```

Linux produces `target/chromium/tilo-chromium-host` and adjacent CEF resources;
run the browser against it with `TILO_CHROMIUM_HOST` set to that path. On Tilo
OS the engine is the signed runtime bundle `tilo-runtime/2026.10.23-chromium`
(`ui::webview::chromium::runtime`): the image build runs
`build-host.py --sdk-archive … --runtime …` and signs the resulting tree, the
browser's manifest lists it under `optional-runtimes`, and the launcher mounts
it read-only into the browser's sandbox.
Linux requires Chromium sandbox support (user namespaces or the distribution's
correctly installed setuid sandbox). The host never disables the sandbox. The
primary process skips Chromium first-run UI because Tilo owns onboarding; this
also avoids an offscreen initialization stall on Linux.

Run with `--profile=/absolute/profile/directory`. Each concurrently running host
must use its own profile directory. The profile is canonicalized to avoid
macOS `/var` versus `/private/var` cache path mismatches.

Browser processes that use one profile therefore share one host. The first
process to open the profile holds `<profile>.lock`, starts the host and serves
`<profile>.sock` beside the profile directory; every later process connects
there and drives its own tabs through that host (see
`crates/sdk/ui-core/src/webview/chromium/shared.rs`). The owning process calls
`ui::webview::chromium::serve_shared_until_idle()` after its event loop exits,
so the host outlives every process still using it.

## IPC

stdin/stdout are a binary protocol, not a terminal. Each packet starts with a
little-endian u32 length including a one-byte kind. Kind 1 carries UTF-8 JSON;
kind 2 carries u32 width, u32 height, then exactly width × height × 4 BGRA bytes;
kind 3 is a frame of another tab, its u32 tab first. Kind 4 (with
`--tilo-frame-damage`) changes a tab's last frame: u32 tab, width, height and
rect count, then each rect's u32 x, y, width and height, then each rect's rows
of BGRA pixels in turn.
The limit is 64 MiB per packet, with dimensions 1–8192 and checked frame sizes.

Whole frames need not go down the pipe. Started with `--frame-memory-fd=N`, the
host maps the memory that descriptor names (`src/frame_memory.h`: a header page
of per-slot state words, then two 64 MiB slots), closes it before anything else
opens a descriptor, and copies each whole frame's BGRA pixels into a free slot.
It marks the slot ready and sends kind 5: u32 tab, width, height and slot.
Damage packets stay in the pipe: they are small. The
reader copies the pixels out and marks the slot free; while both slots are
being read the writer waits for one, still sending events. The app creates the
memory, a memfd sealed against resizing on Linux and an unlinked POSIX shared
memory object on macOS, and the host inherits it at spawn; a host without
the flag, or given a descriptor of any other size, pipes its frames.

JSON commands use `op`; events use `event`. Supported operations are navigate,
back, forward, reload, stop, resize, focus, mouse_move, mouse_button, scroll,
key, ime_composition, ime_commit, ime_cancel, ime_finish, scripts,
filter_rules, clear_data, devtools, page, shutdown. `filter_rules` takes
`rules` text and `enabled`: a boolean replaces the toggle, while null preserves
the current toggle. Compilation failure retains the previous rules and reports
an error. The shared protocol specification is in the browser implementation
plan. Invalid packets or command fields and stdin EOF close the browser.
Diagnostics use stderr or the profile's `cef.log`. Resize dimensions, pointer
coordinates and wheel deltas are physical pixels; the host converts them to CEF
logical coordinates using `scale_factor`.

Native editing history advertises `native_edit_history: true` in each tab's
`ready` / `tab_ready` event. A host without this flag never receives the new
`edit_history` command. Its only fields are `op`, `tab` and `direction`, where
`direction` is exactly `undo` or `redo`; extra fields are rejected. It invokes
CEF's native focused-frame Undo/Redo, without JavaScript or simulated keys. The
retained WebView offers this through the system `LocalUndo` capability, so the
same Edit operation services keyboard, menu and automation requests. Command
queue admission is not a claim that a history entry existed or completed.

Each tab's `ready` / `tab_ready` event also advertises `document_size: true`.
Only then may a tab receive `document_size` with exactly `op`, `tab` and a
boolean `enabled`. While enabled, the host reads the page's layout metrics over
DevTools after each paint (one read in flight, one more after it when painted
meanwhile) and sends a `document_size` event with `width`, `height` (the
content size in CSS pixels, never less than the view) and `scroll_y` whenever
any changes. The page can neither observe nor answer it. An embedder sizing the
view to its content (Mail's reader) enables it; it is off by default.

`ready` also advertises `selection_text: true`: a tab may then receive
`selection_text` with exactly `op`, `tab` and a positive `request_id`, and the
host answers with a `selection_text` event carrying the same `request_id` and
the frame's selection as `text` (empty when nothing is selected, at most 1 MiB).
An offscreen engine cannot reach the system clipboard; the embedder copies it.

### Keys and input methods

`key` carries the key's logical name or text in `key` and, optionally, its
place on the keyboard in `code`, a DOM `KeyboardEvent.code` for the keys a
layout changes (`KeyA`–`KeyZ`, `Digit0`–`Digit9`, the US punctuation keys,
`IntlBackslash`, `Space`; another key's code, at most 32 letters and digits,
leaves the key to its name). The place decides the
native key code, so a page's `code` is the key pressed: German z arrives as
`z` on `KeyY`. A Latin letter or digit keeps its own key code, as Chromium
gives it on each layout; a key whose text has no US key (Cyrillic, Greek, an
umlaut) takes the key code of the US key in its place, so Ctrl+С on a Russian
layout is the page's Ctrl+C, and Command+С copies on macOS. Extension
shortcuts name the same Latin letter.

`ready` / `tab_ready` advertise `ime_composition: true`. Such a host takes the
platform input method's composition into the page's focused field as a native
field does:

- `ime_composition` with `text` (at most 4096 UTF-8 bytes) and
  `selection_start` ≤ `selection_end`, the composition's caret in UTF-16 units
  within `text`, sets the text being composed (`ImeSetComposition`). The page
  sees `compositionstart`, then one `compositionupdate` per preedit.
- `ime_commit` with nonempty `text` (at most 64 KiB) replaces the composition
  with it (`ImeCommitText`): `compositionend` carries the committed text. With
  nothing composed it goes in at the caret, as typed.
- `ime_cancel` withdraws the composition (`ImeCancelComposition`);
  `ime_finish` keeps it as it stands (`ImeFinishComposingText`).

The host reports where the person types, so the window places the input
method's candidates beside the text instead of at the view's corner:

- `text_field` says whether the focused node takes text (`editable`), and when
  it does its `purpose` (`text`, `password`, `email`, `url`, `phone`, `number`,
  `search`), `multiline` (text areas and editable documents) and, when the main
  frame's widget can place it, its box: `x`, `y`, `width`, `height` in physical
  pixels. The renderer reports it on each focus change
  (`CefRenderProcessHandler::OnFocusedNodeChanged`, `CefDOMNode` bounds); the
  host asks again when the selection changes and once a scroll or resize has
  settled, and sends it only when it changes. A field in a frame of another
  site goes without a box; read-only fields do not take text.
- `ime_composition_bounds` carries the box of the composition's first line, in
  the same pixels, from `CefRenderHandler::OnImeCompositionRangeChanged`, and
  no box once the composition ends.

The WebView reads them as the field's input purpose and caret: the
composition's box while composing, else the field's first line. Password
fields never reach the input method; their keys go to the page as typed. With
a host that takes compositions, ordinary input method commits reach the page;
the guarded dictation path below is unchanged. The real-host regression sends
a composition to a page input and checks its events, its committed text, both
boxes and the physical key codes:

```sh
python3 apps/browser/chromium/tests/ime_composition.py target/chromium/tilo-chromium-host
```

`--tilo-sealed-documents` starts a host for sealed documents (Mail's reader;
`ready` says `sealed_documents: true`). Its pages never navigate: a main-frame
navigation to anything but `data:` or `about:blank` is cancelled, and when the
person followed a link (a user gesture, not a redirect) it is sent as `open_tab`
for the embedder to open elsewhere; a refresh, a script-less redirect or a frame
goes nowhere, and a popup opens only from a gesture. Network prediction is off,
and the address under the pointer is sent as `link_status` with `url` (empty when
the pointer leaves the link).

Resource navigation uses `scoped_navigation` with a strictly increasing positive
signed-32-bit `request_id`, a `command` (`navigate`, `back`, `forward`, `reload`,
or `history_entry`), and the command's URL or history snapshot/entry fields.
`cancel_navigation` invalidates the matching owner. `navigation_outcome` echoes
that owner, the actual final URL, and an optional failure. Redirects retain the
owner; bootstrap and legacy visual load events cannot complete an owned action.
Renderer termination fails affected owners explicitly; its visual event cannot
invalidate a subsequently admitted recovery. Helper closure fails outstanding
requests. Navigation waits for document-start script registration and the prior
native lifecycle, and replacements stop the prior load without crediting its
callbacks to a new action.

Every tab starts on `about:blank` before its first address. That first entry is
no page the person visited: `history` reports no `back` while it is all that is
behind the page, `back` does not go to it, and `navigation_history` leaves it
out. Once the first page commits, the session history forgets it
(`Page.resetNavigationHistory`), so the page's own `history.back()` cannot reach
it either and `history.length` starts at the page.

A page's own questions (`alert`, `confirm`, `prompt`, and "leave this page?"
from a `beforeunload` handler) are the embedder's to show. `ready` /
`tab_ready` advertise `js_dialogs: true`; a tab that then receives
`js_dialogs` with a boolean `enabled` gets each question as a `js_dialog` event:
a positive `id`, `kind` (`alert`, `confirm`, `prompt` or `beforeunload`), the
asking page's http(s) `origin` (empty for any other page), its `message` and a
prompt's `default_text` (each at most 16 KiB), and for `beforeunload` whether
it is a `reload`. The page waits until `js_dialog_reply` with that `id`, a
boolean `accept` (OK, or Leave) and a prompt's `text` (at most 64 KiB) answers
it. `js_dialog_closed` with the `id` says the page no longer waits, answered or
not (it navigated, or asked another question). A navigation the embedder sends
declines the open question first, as Chromium holds it back while the page
waits. Staying on a page cancels the
navigation that asked to leave it: an owned navigation fails with "The page
asked to stay". A tab that never asked for questions has each declined at once
(a confirm is false, a prompt null) and its page left when it asks to stay, as
closing a tab always leaves it; no page waits on a question nobody sees.

CEF callbacks enqueue output without pipe writes. Events have a bounded queue;
only each tab's newest pending frame is kept, and tabs take turns, so a page
painting every frame never drops another tab's last frame. A paint's pixels are
copied once, during OnPaint, straight into the packet that is sent, which the
tab keeps to send again; no CEF-owned pixel pointer or GPU handle survives the
callback. Dropdown popup pixels are composited into a copy of the main image
before emission. A blocked consumer cannot stall the CEF UI thread; a closed
pipe triggers browser shutdown. In a shared host the owning process passes a
client tab's frame on as it arrived, BGRA in the buffer it was read into; only
the process that draws a frame converts it, as it copies the frame out of the
frame memory; the owner's outbox, too, serves its client's tabs in turn.
`tests/frame_transport.py` prints what the host's process and the reader spend
per frame of a full-screen animation, through the pipe and through frame
memory, for the first tab and a routed one, and checks that every change of a
still tab beside it arrives and that two animating tabs both get their frames.

With `--tilo-frame-damage`, which the toolkit always passes, a paint after a
tab's first goes as a damage packet (kind 4): the rects Chromium repainted
(`OnPaint`'s dirty rects, at most 16, else their bounding box) and their
pixels, cut from the frame the tab keeps when the writer takes it. The writer
keeps the rects of every paint a newer one replaced, and sends the frame whole
when it is the tab's first, its size changed, a popup is composited onto it,
the rects cover half of it, or the app asks (`resend_frame`, when it lacks the
frame they change). The toolkit applies the rects to the frame it keeps whole,
converts only their pixels, and repaints and uploads only them.
`tests/frame_damage.py` checks that a blinking caret arrives as small rects
inside its field, that the frame rebuilt from them is the frame the host sends
whole, and prints the bytes per blink each way.

### Page clipboard

On Linux the engine runs with no display server (`ozone-platform=headless`),
and Chromium's clipboard there is the engine's alone: a site's copy button, a
script's `navigator.clipboard.writeText`, `document.execCommand('copy')`, the
person's Ctrl+C and an extension's offscreen document all copied into it and
no further, and a paste got only what the engine copied. The host bridges it
(`src/page_clipboard.h`). `ready` (and `tab_ready`) then say
`page_clipboard: true`, never for sealed documents, and an embedder that
takes the page's clipboard sends `page_clipboard` (exactly `op` and `tab`)
for a tab; a tab of one that does not keeps the engine's own.

- Each renderer runs `src/page_clipboard.js` in every web document and
  extension page before the page's scripts. Chromium decides what may copy,
  as in Chrome: a copy or cut event fires only for a copy that may happen
  (and a script's own event copies nothing), and `writeText` and `write`
  resolve only once allowed. What the copy took (the selection with its
  markup, the focused field's selected characters, or what a copy listener
  set and cancelled the event for) goes to the host, which sends it as a
  `clipboard` event with `text` (at most 4 MiB), `html` when it has markup
  (at most 1 MiB, dropped when larger) and `secret`: true for a copy from a
  password or one-time-code field, or of a password field's value, which the
  Browser copies as a secret (kept out of clipboard history and device
  sync). The host sends a page's copy only within five seconds of the
  person's click, key or menu choice in the tab, as Chrome's activation
  lasts; an extension's page with `clipboardWrite` copies at any time. A
  page's popup and an extension's panel copy through the page they were
  opened from.
- A paste key (Ctrl+V, Ctrl+Shift+V, Shift+Insert) asks the embedder for the
  system clipboard's text with `clipboard_read` (`request_id`), answered by
  `clipboard_contents` with that `request_id` and `text`, or no `text` when
  it holds none. The renderer keeps the text for the paste, then the key
  goes on to the page, which sees its keydown as ever; the trusted paste
  Chromium starts is replaced by one carrying the system clipboard's text
  for the page's listeners, inserted into the focused field when no
  listener took it. Keys pressed meanwhile follow in order. An answer that
  does not come within three seconds leaves the paste to the engine.
- `navigator.clipboard.readText` and `read` ask the person as Chromium does
  (`permission_prompt`, `clipboard`); once allowed, they read the system
  clipboard's text the same way. The host asks the embedder only for a page
  the person can see, of a site whose clipboard setting allows reading (or
  an extension's page with `clipboardRead`), whatever the page's script did.
- An extension's offscreen document (`chrome.offscreen`, how MV3 extensions
  copy) is no browser of the host's, so no renderer callback reaches it.
  For an extension with `offscreen` and `clipboardWrite`, the tab script
  asks the host once the document is made, and the host runs
  `page_clipboard.js` there over the pipe with the worker's token: a copy
  reaches the bridge (`clipboard.write`) and goes as `clipboard` through the
  page the person used last.

`tests/page_clipboard.py` covers the page and the person (a read the person
allowed is not covered there: that node's Chromium refuses a site's read
without asking); `tests/extension_tabs.py` the offscreen document.

## macOS packaging and sandbox

The application follows CEF's Frameworks directory layout, including each
Helper app variant and the framework's versioned symlinks. The helper initializes
`CefScopedSandboxContext` before `CefScopedLibraryLoader::LoadInHelper`, as
required by modern CEF. The browser process uses an NSApplication subclass
implementing CefAppProtocol. An explicit subprocess path accommodates the
intentional difference between the app name and executable name. Explicit
`main_bundle_path` and `framework_dir_path` resolve from the native executable,
so embedding the engine under the Rust Browser.app does not make CEF search the
outer application for its resources. Local builds
are ad hoc signed; distribution signing/notarization is separate. The generated
framework copy is recreated on each link to keep upstream symlink creation
idempotent. Verify a built/rebuilt bundle with
`python3 apps/browser/chromium/tests/macos_bundle.py <app-path>`.

## Tests

```sh
c++ -std=c++17 -Iapps/browser/chromium/src \
  apps/browser/chromium/tests/protocol_test.cc -o /tmp/tilo-protocol-test
/tmp/tilo-protocol-test
python3 apps/browser/chromium/tests/smoke.py \
  'target/chromium/Browser Engine.app/Contents/MacOS/tilo-chromium-host'
python3 apps/browser/chromium/tests/lifecycle.py \
  'target/chromium/Browser Engine.app/Contents/MacOS/tilo-chromium-host'
```

The smoke test navigates to `https://tilo.so` by default and requires ready,
HTTP 200, a nonempty frame, the closed event and exit 0. `--url` selects another
HTTPS page. Lifecycle cases test EOF, truncated framing, invalid lengths,
malformed JSON, wrong packet kinds and malformed command fields. Startup-time
shutdown may take several seconds while Chromium's subprocesses unwind.

Configured scripts execute at document start in direct HTTP/HTTPS documents.
The host enables the DevTools Page domain and waits for document-start script
registration acknowledgments before executing queued navigation commands.
Explicit configuration updates also apply to the current document. Trusted
script bodies execute directly rather than through page `eval()`, so updates
work under strict script CSP while page-authored eval remains blocked. Internal pages, extensions,
file/data URLs, inherited about:blank/srcdoc and blob documents are excluded.
Execution is deduplicated by document and script configuration; reload
and cross-site renderer replacement receive the current configuration once.

The runtime regression suite exercises those boundaries, key codes and editing,
and Retina frame dimensions/pointer coordinates:

```sh
python3 apps/browser/chromium/tests/native_regressions.py \
  'target/chromium/Browser Engine.app/Contents/MacOS/tilo-chromium-host'
```

## Page actions

`page` reads or operates the main document for the browser's `browser.page`
actions and answers with exactly one `page_result` carrying its `request_id`.
`action` is `outline`, `click`, `fill`, `select`, `press` or `submit`.
`src/page_automation.js` runs in a DevTools-created isolated world of the main
frame: it shares the DOM, but page script cannot see or call it (the regression
suite checks this from the main world). An outline lists landmarks, headings,
text and interactive elements; interactive elements carry refs
`d<document>e<element>`. Documents are numbered across the host's tabs, so a ref
never resolves in a later document or another tab. Password and card-number
values are reported by length only. Clicks and keys go through CEF's own input
path, so pages see trusted events with user activation; fill, select and submit
edit the DOM and dispatch the page's input, change and submit events.

An action that operates the page answers once the page has handled its input,
with `navigation` saying what that did. `started`: the page began a main-frame
navigation, or planned a form submission, while handling it; the answer then
waits for that load (up to ten seconds) and carries the new URL, `navigated` and
`loading`. `possible`: it had not, and may still navigate on its own; a click
grants the user activation Chromium attributes a later navigation to. Such a
navigation is not the action's answer: it arrives in the `network` log as its
document request, marked `navigation` with Chromium's `user_gesture`. `none`:
the action ran none of the page's code (reading, reader view).

Not yet covered: cross-origin and same-origin iframe content (listed as
`iframe` lines), file inputs, and drag gestures.

```sh
python3 apps/browser/chromium/tests/page_actions.py target/chromium/tilo-chromium-host
```

### Private guarded text input

`text_input` is a separate backend protocol; it does not activate page-agent
logging. `prepare_text_input` leases the currently focused editable element and
selection, returning only a random opaque `data.lease`. After explicit desktop
consent, `start_text_input` validates that same original snapshot and starts one
bounded recording session without changing its lease or edit stamp. The host
waits for that acknowledgement before launching recognition. `check_text_input`
validates that same lease without acquiring a replacement. `commit_text_input`
requires that lease and at most 4096 UTF-8 bytes of `text`;
`cancel_text_input` retires it. Each request has a positive `request_id` and
answers with a private `text_input_result`, rather than `page_result`. The SDK
adds a five-second admission expiry; late requests fail before editing. Private
commands are never retried after an ambiguous renderer response.

Warm preparation expires after three minutes. Explicit consent can activate the
unchanged original lease once, with a fixed three-minute active-session bound;
checks and repeated starts cannot extend it. The combined warm/active lifetime
is bounded by six minutes, matching the native preparation/consented session
phases. Cancellation after start acknowledgement revokes only that original
receipt scope. Focus loss, navigation, input, composition and observed eligibility
changes retire the lease; composed shadow and same-origin frame ancestors participate in eligibility,
and transient style/class masking or read-only changes also retire it. A stale
cancel cannot retire a successor. Final
validation includes the current value and selection. Page `beforeinput` handlers
may cancel the edit; their changes are revalidated before Chromium's synchronous
`insertText` command. That path preserves native undo/redo. There is no fallback
to setting a value, focusing another field, clipboard or synthetic keys.

Private preparation probes focus in bounded isolated frame contexts without
extracting field content. Subsequent checks, start and final stay pinned to the
original frame ID, loader and context; replacement refuses the old lease. A
failed non-editing check can observe frame retirement with a read-only frame-tree
query, without retrying an evaluation. Ambiguous commits remain unknown.
Cross-origin/opaque ancestors and unobservable contexts remain closed; no
universal-access world, origin bypass or debugger pause is requested. Private
text operations also grant no site transient user activation. Public
page-agent actions retain their existing main-frame contract. See the
[upstream frame-context protocol](https://github.com/ChromeDevTools/devtools-protocol/blob/master/pdl/domains/Page.pdl).

Real-host regressions cover text inputs, textareas, open-shadow inputs, same-origin
iframe fields and rich contenteditable selections, including Unicode replacement, undo/redo, changed
focus/content/selection, cancellation, navigation and protected-field rejection:

```sh
python3 apps/browser/chromium/tests/guarded_text_input.py target/chromium/tilo-chromium-host
```

Composition observation is registered in the private text-input world at
new-document creation, before queued navigation is released. Missing or active
state refuses admission, including preedit before the first private Prepare.
Synthetic end events cannot clear a trusted native start. The actual Blink
composition probes additionally use a test-only inspector in a private network
and PID namespace, with an empty hardware device tree. Production opens no
inspector port. On Linux, after building the host:

```sh
bwrap --unshare-all --die-with-parent --new-session \
  --ro-bind /usr /usr --ro-bind /lib /lib --ro-bind /lib64 /lib64 \
  --ro-bind /etc /etc --proc /proc --dev /dev --tmpfs /tmp \
  --ro-bind "$PWD" /work --chdir /work --clearenv \
  --setenv LANG C.UTF-8 --setenv HOME /tmp \
  --setenv TILO_TEST_PRIVATE_NAMESPACE 1 \
  -- /usr/bin/python3 apps/browser/chromium/tests/native_composition.py \
  target/chromium/tilo-chromium-host -v
```

The SDK now exposes a private deferred receipt to the compositor's native field
bridge. A queued final waits for any outstanding check of its original lease;
focus loss and canceled receipts prevent unadmitted insertion. The compositor
keeps the complete private transcript until acknowledgement. Refusal or unknown
delivery enters ten-minute recovery with explicit Copy/Discard; it never retries
or retargets automatically. Standalone Wayland runners now consume the same
contract through `tilo_text_input_v1`: content-free original-field stamps and
operation ACKs accompany standard text-input-v3 final delivery. Preparation
enables the original deferred field for voice only; consent waits for the
renderer start ACK before recording. The marked final is consumed before public
app event dispatch, and focus changes or late cancellations cannot redirect it
to a successor. With a host that does not take compositions
(`ime_composition`), ordinary composition and OSK remain closed outside this
voice session. Real Wayland protocol and bound native App dispatch tests qualify those
separate production boundaries; a complete standalone-window/microphone journey
remains an acceptance gate.

The mounted `browser-text-input-qualification` example exercises the real
retained `Framed` host with both a direct `WebView` and a production `Decorator`
wrapping it, against a synthetic HTTP fixture. Both run the same journeys; the
wrapper must preserve the inner private receipt and deferred acknowledgement
requirement. All 48 mounted journeys pass, including search, telephone, URL and
plaintext-only fields with start/final ACK, Unicode selection replacement and
native undo/redo. It requires an owned disposable profile and the actual CEF helper. Run it as a non-root user
(the same Chromium sandbox policy as the backend suite):

```sh
python3 apps/browser/chromium/tests/mounted_text_input.py target/chromium/tilo-chromium-host target/debug/examples/browser-text-input-qualification
```

Build the SDK example in the same execution environment as the helper. Backend
fixtures and mounted SDK fixtures prove distinct boundaries; they do not prove an assembled physical
microphone journey. Chromium's one-batch rich-text insertion preserves inline
bold markup and native undo but can convert an adjacent space to `&nbsp;`; the
fixture asserts its exact HTML and exact undo/redo, alongside native typing.
Cross-origin frame ancestry, fields without a DOM selection API (including
email/number inputs), closed shadow roots and an already-active composition at isolated-world
startup need additional renderer lifecycle support before admission.

What an agent host relies on — the process budget (the browser, its storage
service and one renderer per site; Linux adds its sandbox zygotes), hidden
pages painting nothing yet answering page actions, and the paint stream's
`paint_capture` / `paint_resource` ops:

```sh
python3 apps/browser/chromium/tests/agent_host.py target/chromium/tilo-chromium-host
```

## Native blocking

A request-context handler checks HTTP(S) resources, including dedicated-worker
and service-worker requests, against a local adblock-rust instance. Request initiator origin
and resource type are retained; subdocument navigation falls back to the parent
frame rather than mistaking the destination frame for the source. Compiled snapshots are published atomically;
requests already in flight retain their original snapshot. No request waits for
Rust UI IPC. Exceptions, replacements and rewritten URLs use the engine result.
Redirect resources are served from owned bytes at the original request URL.

Renderer-local matchers hide domain-specific and generic CSS selectors using
constructed adopted stylesheets, including under strict page style CSP, while
preserving the page policy and its own stylesheets. They observe class/id changes
and newly inserted nodes. Disabling protection disconnects the
observer and removes its sheet. Cosmetic native callbacks are held in private
closures and expose no privileged global binding. Cosmetic/scriptlet code runs
only in direct HTTP(S) documents. Scriptlets use the bundled upstream resources,
with required dependencies and the upstream `scriptletGlobals` lexical state.

The resource bundle is regenerated and verified using:

```sh
python3 scripts/browser/update-blocker-resources.py --check
python3 apps/browser/chromium/tests/blocking_regressions.py \
  'target/chromium/Browser Engine.app/Contents/MacOS/tilo-chromium-host'
```

The committed bundle includes pinned source hashes, licenses and provenance in
`crates/browser/chromium-blocker/resources/`. It has 214 resources. The
parameterized `click2load.html` redirect is unsupported; 34 upstream trusted-only
resources retain permission mask 1 and cannot be invoked by ordinary lists.
This is not a claim of complete uBlock Origin compatibility: procedural cosmetic
actions are not executed, generic class/id collection is bounded, and the initial
cosmetic/scriptlet configuration may arrive after parser scripts in a new renderer.
The separate custom `scripts` command has verified document-start behavior.

The packaged nesting regression is separate from standalone runtime checks:

```sh
python3 apps/browser/chromium/tests/nested_bundle.py \
  'target/browser-package/Browser.app/Contents/Frameworks/Browser Engine.app/Contents/MacOS/tilo-chromium-host'
```

It requires the nested location and verifies initialization, local HTTP navigation,
frame delivery, and clean shutdown using the packaged executable.

## Native navigation receipt regression

```sh
cargo run --locked -p browser --example native_navigation_completion
```

On Linux this builds the pinned, digest-verified engine with the maintained host
helper and drives the real Browser's registered actions against an isolated local
HTTP fixture. It verifies trusted fragment-link Back/Forward receipts, one durable
history visit per completion, a concrete refused-connection failure, and recovery
followed by the same history route. Chromium runs unprivileged with its sandbox
enabled. A fixture supervisor reaps its descendants before the ordinary isolated
test profile is removed. Missing engine dependencies or observations fail the run.
`--host PATH` selects an existing runtime's exact executable and reports its
SHA256 while preserving the same unprivileged supervisor and sandbox. The
fixture also checks startup navigation ownership, redirects, repeated URLs,
explicit internal-URL rejection at the Browser boundary, and native-engine
blank/canonical URL, cancellation/recovery, history refusal, and renderer
termination followed by Reload.

## Extensions and tabs

Chromium finds an extension's tabs through the tab strips of its own windows.
Every page here is an offscreen (Alloy-style) browser no window holds, so
`chrome.tabs.query` and `chrome.windows.getAll` find nothing, `scripting` and
`tabs.sendMessage` find no tab by its id, and a toolbar click has nowhere to
come from. Content scripts, declarative network rules and messages from
content scripts (with `sender.tab`) work without help.

`src/extension_tabs.h` answers the rest from the host's own tabs:

- `src/extension_tabs.js` runs in each extension's service worker (the Browser
  makes the worker run it first, `extensions.rs` `prepare_tabs`, without
  moving the worker: extensions read their worker's address and load their
  helpers against it) and in its own pages (a document script). It answers
  `tabs`, `windows`, `action.onClicked`, `contextMenus`, `notifications`,
  `omnibox`, the button's look for one tab and `scripting` by asking the
  host at
  `https://extension-bridge.tilo.invalid/`, which the host serves itself. A
  page of the extension is known by its frame's address, its worker by a token
  written into it; a page cannot show either. An extension whose security
  policy lists the only servers its pages may connect to, in its manifest or
  in a page's `<meta>` (password managers do), gets the bridge added to that
  list (`extensions.rs` `policy_with_bridge`), and stays turned on. A failed
  call's callback finds `runtime.lastError` set, as in Chrome.
- Tabs carry the id Chromium's extension APIs know them by, the one in
  `sender.tab.id`, from their first page on: a blank page, an extension's
  own page in a tab and an internal one have it too, and keep it once they
  show a web page. Chromium lists it with each tab's DevTools target to an
  extension that may debug; the Browser's own extension (`extension-manager`)
  may, and the host asks its worker for every tab not yet known at once,
  before an extension is told of one. An extension's own page is listed
  with no tab: it sends that extension a word only the host made, and
  Chromium tells the worker the tab the message came from. An id another
  tab has already is not taken. The page's scripts see its word, so two
  pages of one extension can trade theirs and be taken for each other's
  tab; such an id gives way to the one Chromium lists once the tab shows a
  page it lists with its tab, before any extension asks after that page.
- What a tab shows an extension follows Chrome: its address and title with
  `tabs`, a host permission for the site, or a click on the extension's
  button over it (`activeTab`), which also lets it script the page until the
  page navigates. A click gives an extension without `activeTab` nothing.
  The Browser sends an extension's host permissions apart from the sites its
  content scripts run on (`extension_access`, `hosts` and `scriptable`), and
  no tab API counts the second: as in Chrome, content scripts on every page
  let an extension see no tab's address, script no page and capture none.
- `tabs.detectLanguage` answers the language the page declares, and
  `tabs.getZoom` the page's zoom. `tabs.onZoomChange` fires when a tab's
  zoom changes from its first page on: the Browser zoomed it or another
  page of its site, or it went to a site zoomed otherwise.
- Tab events: `onCreated`, `onUpdated`, `onRemoved`, and on focus
  `onActivated` with `onHighlighted` (the tab alone, being its window's only
  one) and `windows.onFocusChanged`. A tab never moves to another window and
  is never replaced, so `onAttached`, `onDetached`, `onMoved` and
  `onReplaced` never fire.
- `webNavigation` is Chromium's own: its events, `getFrame` and
  `getAllFrames` know each tab by the id the tab APIs give it. Chromium does
  not see a page open a tab here (the Browser opens it), so the host tells
  of it: when the tab the page asked for (a link to a new tab,
  `window.open`) shows its address, it is given the page's tab as
  `openerTabId`, and `webNavigation.onCreatedNavigationTarget` fires to
  extensions with `webNavigation`, after the new tab's first navigation
  events rather than before them. The tab script reads the opening frame's
  id and process from the page's frames, by the frame's address.
- The toolbar button's look for every tab is Chromium's, and so are its
  getters; the script tells the host of each change Chromium accepted. For
  one tab (an ad blocker's count for the page) Chromium finds no tab and
  refuses, so the host keeps the badge, title, popup and state until the
  tab's next document or its close, as Chrome does, and answers for them.
  Either way the host tells the Browser (`extension_action`), for that tab
  alone or for every tab, a tab opened later included, and a new document
  takes back what was set for its tab (`extension_action_reset`). The
  toolbar shows the page's badge, title, icon, popup and state over every
  tab's; a badge colour is taken and not shown, the toolbar's colours being
  the system's. `action.openPopup`, which finds no window here, opens the
  popup over the page in front (`extension_open_popup`).
- `scripting.executeScript` runs in the extension's own world in the page,
  where its content scripts run. Where it has none yet but declares content
  scripts for the page, the script waits up to one and a half seconds for
  Chromium to make it, as their first script runs: AdGuard injects its
  keep-alive on `tabs.onUpdated` as the page starts, before then, and it
  needs their `chrome`. Where it has none, the host makes a world, in the
  main frame and in each frame of the page's process it injects into
  (`allFrames` reaches those the extension may script: a host permission for
  the frame's address, or `activeTab` for the page's origin), that gets a
  content script's `runtime`, `storage` and `i18n`
  (`src/extension_content.js`), through a binding only that world has:
  messages and ports both ways (`runtime.sendMessage` and `runtime.connect`
  from the script, `tabs.sendMessage` and `tabs.connect` from the
  extension), storage and its `onChanged`. What the script sends reaches
  the extension's worker and its open pages alike, as in Chrome, so a popup
  answers when the worker does not. A port's messages go to the host in
  order, batched while a request is under way, and on from the host to the
  worker or page the same way, so a burst wakes the worker once.
- An offscreen document (`chrome.offscreen`, which MV3 extensions make to
  parse HTML, copy to the clipboard or play sound) is Chromium's own: made,
  found (`hasDocument`, `runtime.getContexts`) and closed as in Chrome, it
  has Chrome's one API there, `runtime`, and asks its worker for whatever
  needs a tab. A message from a script the host runs in a page reaches it
  as well: Chromium keeps the document with no tab, so for an extension
  with the `offscreen` permission the host finds it among the pipe's
  targets and delivers the message there, attached for as long as that
  takes. A port such a script opens does not reach it. Its sound plays
  through Chromium's own output, not the audio bridge, which CEF feeds only
  with its browsers' sound: a governed Browser, which has no audio device,
  plays none of it.
- `scripting.executeScript` reaches the frames an extension names
  (`frameIds`, `documentIds`, `allFrames`) and `tabs.sendMessage` one frame
  (`frameId`, `documentId`), as Bitwarden injects its form filler into each
  frame that asks. A message reaches the frame's content scripts, or the
  extension's own page in the frame, as Evernote Web Clipper answers the
  panel it shows in the page. A message that names no frame reaches every
  frame the extension has a world in, as in Chrome, so a password manager's
  script in a login frame or a translator's in each frame hears it: the
  first reply answers it, nothing does once every frame heard it and none
  replied, and no receiver when none listens. It asks no frame its id, and
  of the frames other sites' processes render it attaches only to those the
  extension may have a world in: its own page, a page its content scripts
  match (`scriptable`), or one the host met its world in before; another
  site's frame without one costs no session. Chromium knows a frame and
  its document by ids the inspector does not show, so the host asks the
  extension's world
  in the frame (or its page there) over a port only the tab script answers
  (`sender.frameId`, `sender.documentId`): a port, because an extension
  page without the tab script that answers every message itself cannot
  answer it first. A frame's id lasts as long as the frame, its document's
  as long as the document. A frame another site's process renders is a
  target of its own, which the host attaches to on the pipe for as long as
  the call takes, and only when the frames named are not all in the page's
  own process; a frame named that the host met before on a target of its
  own is the only one attached to, so each message to an extension's panel
  costs one session. The pipe carries a script whole, up to 16 MiB.
  `allFrames` passes over a frame the script cannot run in, as Chrome does:
  the extension's own page, and one that went to another site's page the
  extension's content scripts do not match, which keeps its id but has no
  world of the extension in its new process. A frame the extension has no
  world in cannot say which it is, so the host reads the frames Chromium
  lists for the tab (`webNavigation.getAllFrames`, through the Browser's own
  extension) and places each under its parent, by its id where known, else
  by its address in the page's order; the same list names the document of
  a frame no world told of, so every result carries its `documentId`, as in
  Chrome. The list is read while the script runs, once per document. Another
  site's frame the extension has no world in gets none: the host keeps no
  session to it for the world's messages.
- A worker that asks for the last focused window before the person focused
  a page (it starts with the engine) gets the first page's, as in Chrome. A
  tab an extension was told of while the host could not learn its id among
  Chromium's (the Browser's own extension did not start) keeps answering to
  the id it was told.
- `tabs.connect` also reaches an extension's declared content scripts,
  which Chromium cannot find by a tab it does not know: the host gives their
  world what a port needs, through the same binding.
- The worker tells the host of storage changes while such worlds exist, so
  they hear of a change made anywhere; once none is left the host says so
  and the worker stops listening, and is not woken for changes again.
- An extension added while a page is open and running its script there, as
  High Contrast and dark modes do once installed, follows the settings
  changed in its popup; a shortcut a content script takes counts each press,
  on macOS too, where the host stamps the keys it forwards with their time.
  `tests/site_extensions.py` covers both.
- `tabs.captureVisibleTab` encodes the frame the host last painted of the
  tab, PNG or JPEG at the quality asked for (90 by default), off the UI
  thread. As in Chrome it needs a click on the extension's button over the
  tab (`activeTab`) or a host permission for every site (`<all_urls>`), and answers each
  extension at most twice a second. A hidden tab has no frame to capture.
- `contextMenus` items are kept by the host (`src/extension_menus.h`) and
  shown in the page's menu where their contexts (page, selection, link,
  image, video, audio, editable, frame) and URL patterns say, after the
  page's own items, each extension's set apart: checkbox and radio items
  with their check marks, an item with items under it as a submenu, a
  disabled one greyed. Items for the toolbar button (`action`, `all`) go to
  the Browser as `extension_menu` events whenever they change, and to each
  tab as it opens; the Browser shows them in the button's menu and names a
  choice with `extension_menu_action`. Choosing one fires
  `contextMenus.onClicked` with Chrome's info and the tab, and grants
  `activeTab`. The items and their checked state are kept in the profile
  (`Extension Menus.json`, never for a private window), since an extension
  makes them once, when installed. Unlike Chrome, the page menu does not
  gather an extension's items under its name.
- Keyboard shortcuts (`commands`): the Browser tells the host each
  extension's manifest `commands` with its access, in the order the
  extensions were installed, and `src/extension_commands.h` gives each
  command the key suggested for the platform as Chrome does: Ctrl or Alt
  (Command or MacCtrl on macOS), maybe Shift, and one key, never Ctrl with
  Alt; at most four an extension; a key two extensions suggest stays with the
  first; and none the Browser keeps for itself (Find, bookmark, reload, the
  address, its tab keys), which it answers before a page sees them. A key
  the Browser forwards into a page is looked up first: a command's runs it
  and the page gets neither its press nor its release. A named command fires
  `commands.onCommand` with the tab, waking the worker, and grants
  `activeTab`; the button's own (`_execute_action`, or MV2's
  `_execute_browser_action`) asks the Browser to act as a click on the
  button (`extension_execute_action`), which opens its popup or fires
  `action.onClicked`. `commands.getAll` lists the button's command first,
  then the rest by name, each with its key as Chrome shows it ("Ctrl+Shift+Y"
  on Linux) or none, and its description localized. The list of keys the
  Browser keeps is `src/browser_keeps.inc`, and the Browser's own test
  (`browser_window.rs`) presses every key a command could hold through its
  key handling and fails when what it answers and the list differ. In an
  extension's popup or side panel a command's key runs it over the page the
  panel is for, as Chrome's keys belong to the window: the button's own key
  pressed in its open popup closes it. The host tells the Browser which keys
  the commands hold (`extension_shortcuts`), so while its address has the
  keyboard the Browser hands such a key to the host (`extension_command_key`)
  after its own keys and before the field sees it.
- `notifications` are kept by the host (`src/extension_notifications.h`),
  which checks them as Chrome does (a type, an icon, a title and a message;
  a progress from 0 to 100 only for a progress notification; at most two
  buttons) and answers `update`, `clear` and `getAll`. The Browser is told
  of each change through the page used last (`extension_notification`) and
  shows it as its own notification, named for the extension
  (`extension_notices.rs`); a change by `update` (a download's progress)
  shows without a sound, as does one the extension made `silent`. The person choosing it or one of its buttons
  comes back as `extension_notification_chosen`: the worker, woken if it
  sleeps, hears `onClicked` or `onButtonClicked`, then `onClosed` by the
  person, since Tilo takes a chosen notification away; `clear` is heard as
  `onClosed` not by the person.
- `chrome.permissions` (`src/extension_permissions.h`): Chromium asks the
  person for an extension's optional access in a dialog of its own window,
  and answers "Could not find an active window." here. The Browser tells
  the host what each extension's manifest offers for later
  (`optional_permissions`, `optional_host_permissions`) and what of it the
  person granted (`extension_grants`); `request`, made in answer to
  something the person did and only for what the manifest offers, asks the
  Browser (`extension_permission_request`), which asks the person in its
  shared prompt, granting at once what Chrome asks nobody about, and
  answers (`extension_permission_decision`). What the extension holds then
  counts for its tabs, scripting, capture and menu items at once, it hears
  of it (`onAdded`, `onRemoved`) in its worker and pages, and the Browser
  keeps it with the extension (`extension_permissions`). `contains`,
  `getAll` and `remove` (never of what the manifest requires) answer from
  the same. An API Chromium itself gives only an extension that holds it
  in Chromium's own records (`chrome.bookmarks`) stays missing when the
  person grants it here: what the host answers follows the grant, and the
  tab script gives `chrome.identity` to an extension that may ask for it
  later, which signs in once granted. The person sees what they granted in
  the extension's Library row and revokes it there or through its resource
  (`revoke-extension-access`): the Browser sends `extension_grants` without
  it, which holds at once, and the extension hears of what it lost
  (`onRemoved`). A question whose tab closes unanswered, its popup or the
  window's page, is answered no, so the extension may ask again.
- `downloads.download` goes through the Browser's own downloads
  (`src/capabilities.h`): Chromium cancels a download an extension starts
  on none of its windows' tabs, so the host starts it on the page the call
  came from or the one in front, and the person is asked as for any
  download, which also answers `saveAs`. The extension gets the download's
  id once it started (the first download on that page from the address it
  asked for), and it is saved under the last part of `filename`, in a
  folder of its own like every download here. An address is a web one,
  `data:` or a blob of the extension's own pages; a request body or headers
  of its own are refused. Everything else (`search`, `pause`, `resume`,
  `cancel`, `erase`, `onCreated`, `onChanged`) is Chromium's, which sees
  every download of the profile. `open` and `show` ask the Browser showing
  the page (`extension_download_open`), which opens a complete download, or
  shows it in its folder, as its own Open and Show in Files do; `open`
  needs `downloads.open` and, as in Chrome, the person's click or key in
  the extension's page, or on its button, menu item or shortcut, in the
  last five seconds. A page of the extension in a frame of a web page
  counts a click or key in that frame, not one elsewhere on the page (the
  focused frame, or one it is in, is the extension's), and so does
  `sidePanel.open`.
- The address's keyword (`omnibox`): the Browser reads each extension's
  manifest `omnibox.keyword`, and what the person types after it and a
  space, in the window's bar or the window's own address field, is the
  extension's (`extension_omnibox`): the host fires `onInputStarted`, then
  `onInputChanged` for each change with the request its suggestions answer,
  and `onInputCancelled` when the person leaves, waking the worker. What the
  extension suggests for the latest change (`extension_omnibox_suggestions`)
  and its default suggestion (`extension_omnibox_default`, `%s` standing
  for what is typed) go to the page's tab, and the Browser lists them, plain,
  under the address, the default first. Enter, or a suggestion chosen, fires
  `onInputEntered` with its text and `currentTab`, makes that window the
  extension's current one and grants `activeTab`, as Chrome does; the
  extension sends the page where it says (`tabs.update`). The extension's
  resource offers the same as `send-to-extension`, for scripts and agents.
- Side panels (`sidePanel`): Chromium keeps an extension's options, for
  every tab and for one, and how a click on its button treats the panel
  (`setPanelBehavior`), and answers for them; it finds no window to show a
  panel in. The script asks the host (`sidePanel.show`), which asks the
  Browser (`extension_side_panel`) to show the page the options name beside
  the tab's page, where the Browser lays it out at the window's trailing
  edge, never over the page. `sidePanel.open` needs a gesture, as in
  Chrome: it runs from a listener of something the person did (a click, a
  keyboard shortcut, a menu item) while it runs, or from the extension's own
  page the person used; the host also refuses one more than five seconds
  after the person's last act for the extension. A click on its button
  opens or closes the panel instead of reaching the extension when it asked
  for that (`openPanelOnActionClick`) and its button has no popup, as in
  Chrome; the worker wakes for the click and answers which page.
  `setOptions` reaches the panel where it shows (another page, or closed
  once turned off for the tab) and `sidePanel.close` closes it. The person
  opens a panel from the Browser too (`extension_side_panel`, the page the
  extension names for the tab). The panel is a panel tab, as a popup is: no
  tab to the extension, its window the page beside it. The Browser marks it
  as the extension's side panel (`extension_panel` with `side_panel`), and the
  first of the extension's pages it shows fires `sidePanel.onOpened`, its
  closing `sidePanel.onClosed`, each with Chrome's `windowId`, `tabId` (both
  the page's tab, each tab being a window of its own here) and the `path` of
  the page it showed; another page in the open panel keeps it open. The
  person drags the edge between the page and the panel, or steps it from the
  keyboard or assistive technology, between a utility panel's width and a
  readable column's, leaving the page a readable column, and each
  extension's panel keeps its width on the device.
- A call from an extension's page is a request the page cancels as it
  closes, where Chrome has sent it already. A popup that acts and closes at
  once (`tabs.reload(id); window.close()`, as Privacy Badger's "Disable for
  this site" and uBlock Origin Lite's element zapper do) closes once its
  calls are answered, or after two seconds.
- Network rules (`declarativeNetRequest`) block and redirect in the
  Browser's tabs, but Chromium counts what they did there for no tab (its
  rules see the tab id -1), so it shows no count on an ad blocker's button
  and answers for no tab. The Browser's own extension sees each request a
  rule blocked, or redirected to an extension's file, by the tab webRequest
  reports it for, and tells the host (`requests.acted`, with a token the
  Browser writes beside its worker, `extensions.rs` `write_manager`). The
  host (`src/action_counts.h`) keeps them for the page each tab shows, from
  when it began loading, and counts each for the extensions whose button
  shows the page's count (`setExtensionActionOptions` with
  `displayActionCountAsBadgeText`, as uBlock Origin Lite asks). A redirect
  to a file by its extension's id counts for that extension; any other
  request counts for the one extension with network rules, or else for each
  whose own rules match it (`testMatchOutcome`, which Chromium offers here,
  every extension being unpacked to it). An extension that starts showing
  the count on a page, as its setting turns it on or as its first call
  waits for the Browser to say whose worker it is, counts what its rules
  did there so far, as Chrome does. The count reaches the Browser as
  the page's badge for that tab (`extension_action`), under text the
  extension set for the page; it changes as the extension says
  (`tabUpdate`) and goes with the page. The toolbar shows no badge for
  Chromium's placeholder for every tab. `getMatchedRules({tabId})` answers
  an extension with `declarativeNetRequestFeedback`, or `activeTab` granted
  on the tab, with its rules that blocked or redirected requests of the
  page; `onRuleMatchedDebug` reports what Chromium reports, tab id -1.
  Rules that only change a request (headers, an upgrade to https) and a
  blocked page itself are not counted.
- A rule's redirect to an extension's file at its address for the session
  (`use_dynamic_url`, as uBlock Origin Lite's stand-ins for ad and
  analytics scripts are) runs in the page. Chromium answers that address
  with a redirect to the extension's own and allows the file there because
  it remembers where the request came from; CEF starts each redirected
  request anew and Chromium forgets, so it would refuse the file. The host
  (`src/extension_standins.h`) answers that second request itself, with the
  file from where Chromium says the extension is
  (`Extensions.getExtensions`), when the extension's manifest makes it web
  accessible to the page that asked.
- An extension signs in to its service with `identity.launchWebAuthFlow`.
  Chrome shows the page in a window of its own; here the host asks the
  Browser to show it in a panel over the page in front
  (`extension_sign_in`), as a popup shows, under the name of the site it is
  on. The extension's own page that asked (its settings, its popup) goes on
  beneath it, so its call hears the outcome, and shows again once the
  sign-in page closes. The Browser marks the panel's tab as that sign-in's
  (`extension_sign_in_panel`), and the host watches where it goes
  (`src/extension_sign_in.h`): once the page goes to the extension's own
  address, `https://<id>.chromiumapp.org/`, by a redirect, a link or a
  script, the navigation is turned away, the call ends with that address
  and the panel closes. Closing the panel turns the sign-in down ("The user
  did not approve access.", as in Chrome), and so does a second sign-in of
  the same extension for the first. A sign-in that may not ask the person
  (`interactive` false) is loaded out of sight in a tab of its own, as
  Chrome loads it: the site sending the person straight back ends it with
  the address; its page loading without doing so ends it as needing them
  ("User interaction required."), or with `abortOnLoadForNonInteractive`
  false, for a site whose script sends them back later, its time running
  out (`timeoutMsForNonInteractive`, a minute at most). The extension asks
  for the outcome in turns the host holds 20 seconds each, and calls
  Chromium between them, which keeps its worker awake as Chrome keeps it
  awake during the call.
  `getRedirectURL` names the extension's address; `getAuthToken`, which
  signs in with the Browser's own Google account, answers that no one is
  signed in.
- A click or a tab event reaches a worker through the DevTools pipe, which
  wakes it if it sleeps; the host attaches only while it delivers, so an idle
  worker still sleeps. A worker tells the host all it listens for once its
  first script has run. Until it has (it starts with the engine, or as it is
  added, and may ask before the Browser has said whose worker it is), the tab
  events it may want are kept for it, for up to thirty seconds, and it hears
  those it listens for then: a page that loads as the Browser starts is one
  its extensions hear of, as in Chrome. `tests/extension_tabs.py` covers each
  of these.

## Remaining integration work

- Extensions' side panels show beside the page they were opened for, one at a
  time per window; `sidePanel.onOpened` and `onClosed` always name the tab,
  also for a panel the extension opened for the whole window. Extensions'
  keyboard shortcuts run while a page, a popup, a side panel or the Browser's
  address has the keyboard, but not while the address is the Tilo window
  bar's location, which the desktop draws; the suggested keys are the ones
  given (there is no page to change them), and `global` shortcuts work only
  in the Browser. On macOS a shortcut with
  Option (Alt) and no Command or Control does not run: the Browser forwards
  the character Option types ("˚" for Option+K), not the key. A keyword's
  Enter always goes to the current tab (no Alt+Enter for a new one), and its
  suggestions cannot be deleted (`onDeleteSuggestion`). Their context menu
  items show no checkmark and no submenu, and appear only in a page's menu,
  not the toolbar button's.
- `tabs.setZoom` is not offered: the Browser owns a page's zoom.
- An extension's notification shows the Browser's icon, not its own or its
  image, and a notification the person dismisses without choosing it stays
  in `getAll` until the extension clears it: Tilo's notification service
  says nothing of a dismissal.
- A rule's redirect of a frame (not a script or image) to an extension's
  file at its address for the session is still refused.
- CPU BGRA transport is implemented; shared GPU texture import is not.
- `clear_data` currently clears cookies, HTTP cache and HTTP authentication.
  Other site storage, including IndexedDB/localStorage/service workers, needs
  the full clear-data integration.
- Keyboard input covers single UTF-16 characters, common named keys and input
  method compositions, with macOS native and Linux X11 key mappings from the
  key's physical place. Outside a composition the input method is placed by
  the focused field's first line, not its caret: CEF reports a caret only for
  composed text. A field in a frame of another site has no box, and its
  candidates open at the view's corner until the composition reports its own.
- The isolated Linux ARM64 native navigation receipt regression is verified.
  Packaged hardware acceptance and macOS release signing/notarization remain separate checks.
