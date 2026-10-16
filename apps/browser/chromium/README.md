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
OS the engine is the signed runtime bundle `tilo-runtime/2026.10.10-chromium`
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
kind 2 carries u32 width, u32 height, then exactly width × height × 4 BGRA bytes.
The limit is 64 MiB per packet, with dimensions 1–8192 and checked frame sizes.

JSON commands use `op`; events use `event`. Supported operations are navigate,
back, forward, reload, stop, resize, focus, mouse_move, mouse_button, scroll,
key, scripts, filter_rules, clear_data, devtools, page, shutdown. `filter_rules` takes
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

CEF callbacks enqueue output without pipe writes. Events have a bounded queue;
only the newest pending CPU frame is kept. Frames are copied during OnPaint;
no CEF-owned pixel pointer or GPU handle survives the callback. Dropdown popup
pixels are composited into the main image before emission. A blocked consumer
cannot stall the CEF UI thread; a closed pipe triggers browser shutdown.

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
edit the DOM and dispatch the page's input, change and submit events. An action
that starts a main-frame navigation answers once it loads (up to ten seconds),
with `navigated` set and the new URL.

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
to a successor. Ordinary composition and OSK remain closed outside this voice
session. Real Wayland protocol and bound native App dispatch tests qualify those
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

## Remaining integration work

- Browser UI extension management and Chrome extension behavior remain separate
  integration work; this host does not claim extension installation support.
- CPU BGRA transport is implemented; shared GPU texture import is not.
- `clear_data` currently clears cookies, HTTP cache and HTTP authentication.
  Other site storage, including IndexedDB/localStorage/service workers, needs
  the full clear-data integration.
- Keyboard input covers single UTF-16 characters and common named keys with
  macOS native and Linux X11 key mappings. Physical codes use a US layout because
  IPC carries logical names; IME, composed text, other layouts and full native
  shortcut handling need platform integration.
- The isolated Linux ARM64 native navigation receipt regression is verified.
  Packaged hardware acceptance and macOS release signing/notarization remain separate checks.
