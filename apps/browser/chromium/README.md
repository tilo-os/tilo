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

Linux produces `target/chromium/tilo-chromium-host` and adjacent CEF resources.
Linux requires Chromium sandbox support (user namespaces or the distribution's
correctly installed setuid sandbox). The host never disables the sandbox. The
primary process skips Chromium first-run UI because Tilo owns onboarding; this
also avoids an offscreen initialization stall on Linux.
Linux packaging is implemented but has not yet been exercised on Linux.

Run with `--profile=/absolute/profile/directory`. Each concurrently running host
must use its own profile directory. The profile is canonicalized to avoid
macOS `/var` versus `/private/var` cache path mismatches.

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
- Linux runtime execution and macOS release signing/notarization remain unverified.
