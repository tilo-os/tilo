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
SDK and `QOS_OUTPUT_DIR` to the desired runtime directory. C++20 and CMake 3.21+
are required. macOS uses the Apple command line tools; Linux needs a supported
CEF distribution and the system runtime dependencies listed in its README.

The default macOS executable is:

```
target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host
```

Linux produces `target/chromium/qos-chromium-host` and adjacent CEF resources.
Linux requires Chromium sandbox support (user namespaces or the distribution's
correctly installed setuid sandbox). The host never disables the sandbox.
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
key, scripts, clear_data, devtools, shutdown. `filter_rules` currently reports
an explicit error because this development host does not yet link the native
blocker. The shared protocol specification is in the browser implementation
plan. Invalid packets or command fields and stdin EOF close the browser.
Diagnostics use stderr or the profile's `cef.log`.

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
intentional difference between the app name and executable name. Local builds
are ad hoc signed; distribution signing/notarization is separate.

## Tests

```sh
c++ -std=c++17 -Iapps/browser/chromium/src \
  apps/browser/chromium/tests/protocol_test.cc -o /tmp/qos-protocol-test
/tmp/qos-protocol-test
python3 apps/browser/chromium/tests/smoke.py \
  'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host'
python3 apps/browser/chromium/tests/lifecycle.py \
  'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host'
```

The smoke test navigates to `https://podesta.ai` by default and requires ready,
HTTP 200, a nonempty frame, the closed event and exit 0. `--url` selects another
HTTPS page. Lifecycle cases test EOF, truncated framing, invalid lengths,
malformed JSON, wrong packet kinds and malformed command fields. Startup-time
shutdown may take several seconds while Chromium's subprocesses unwind.

## Remaining integration work

- Link the Rust blocker and intercept requests through a request-context handler.
- Browser UI extension management and Chrome extension behavior remain separate
  integration work; this host does not claim extension installation support.
- CPU BGRA transport is implemented; shared GPU texture import is not.
- Script updates reach the current renderer; script persistence across newly
  created site-isolated renderer processes needs browser-to-renderer startup
  synchronization. There are no privileged JavaScript bindings.
- `clear_data` currently clears cookies, HTTP cache and HTTP authentication.
  Other site storage, including IndexedDB/localStorage/service workers, needs
  the full clear-data integration.
- Keyboard input covers single UTF-16 characters and common named keys. IME,
  composed text and full native shortcut handling need platform integration.
- Linux runtime execution and macOS release signing/notarization remain unverified.
