# Task 2 native blocking implementation

## Implemented

- CMake requires and links the Rust blocker static library into the CEF host and renderer helpers. Linux architecture detection recognizes aarch64 as arm64.
- `CefRequestContextHandler` covers HTTP(S) requests, including service-worker requests without frame/browser handles. Resource kinds and source/initiator context reach adblock-rust. Each request retains an immutable compiled snapshot; updates atomically replace it. No per-request UI IPC.
- Network blocks, exceptions, type/party rules, rewritten URLs, and in-memory redirects consume engine output. Replacement streams retain their bytes, including empty replacements.
- `filter_rules.enabled` accepts true/false/null; null preserves the current enabled state. Failed compilation retains the prior rules and emits a visible error.
- Renderer-local matching handles domain/generic CSS selectors, initial document nodes, class/id changes, inserted nodes, and disabling/re-enabling on a live document. Private V8 callbacks have no privileged global binding. Internal/extension/data/blob/inherited documents are excluded.
- Bundled scriptlets execute with dependency functions and lexical `scriptletGlobals`. The set-constant fixture verifies actual execution, not just generated text.
- Custom scripts use CDP `Page.enable`, followed by acknowledged `Page.addScriptToEvaluateOnNewDocument`, with navigation queued until configuration finishes. The Page domain must be enabled even though adding a script without it returns success. Current-document synchronization is once per document/configuration; rapid A→B→A updates re-register the correct source.
- Resource updater pins Brave/uBlock and Brave/adblock-resources commits and checks SHA256 before importing conversion inputs. The 214-entry bundle preserves aliases, dependencies and upstream requiresTrust as permission mask 1. Both licenses and provenance are committed.

## Red/green evidence

Initial real sandboxed CEF tests against the prior host failed: requests reached the local server, CSS display was block instead of none, filter configuration emitted stub errors, and null enabled terminated IPC. These are now green.

Additional failing-first cases:

- Inline parser script saw `early-false` before true document-start registration.
- Scriptlet fixture observed false before supplying upstream lexical globals.
- Rapid A→B→A configuration lost document-start registration before clearing installed state on removal acknowledgment.
- Empty redirect fixture observed no successful empty response before explicit empty-byte resource handling.

## Validation (macOS arm64, CEF 152.0.6 / Chromium 152.0.7977.83)

- `cmake --build target/chromium-cmake -j 4`: successful, ad hoc signed app rebuilt against the Rust static library.
- `python3 apps/browser/chromium/tests/blocking_regressions.py 'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host'`: **9 passed in 29.758s**. Real local HTTP evidence covers script/XHR matching, exceptions, first/third party, dedicated/service workers, CSS initial/dynamic/live toggle, boolean/null state, direct/internal boundaries, custom scripts before inline parser scripts and cross-site navigation, CSP, rapid configuration changes, nonempty/empty redirects and executable scriptlets.
- `ctest --test-dir target/chromium-cmake --output-on-failure`: **1 passed** (protocol).
- `python3 scripts/browser/update-blocker-resources.py --check`: regenerated bundle byte-for-byte; 214 resources; SHA256 `09c1ffe57958dc0eb9974e314e53eee3c4ae70e87457b2db232b0440d1864d6c`.
- `python3 apps/browser/chromium/tests/native_regressions.py <host>`: **4 passed in 7.669s** (keyboard/editing, Retina coordinates, internal-page exclusion, once-per-document/configuration including reload and cross-site navigation).

No sandbox disabling, TLS validation override, or remote debugging port was added. Internal CDP messages do not expose a remote endpoint.

## Repeat-build packaging regression

The parent found strict signature verification failing with ENOENT. The new
`macos_bundle.py` test reproduced three broken framework links after repeated
builds: `Versions/A/A`, `Versions/A/Resources/Resources` and
`Versions/A/Libraries/Libraries`. Upstream CEF's `ln -sf` follows existing directory
symlinks. The native build now removes only its generated framework copy before
recreating the upstream layout. Two consecutive CMake builds both passed a full
symlink walk and `codesign --verify --deep --strict` afterward.

## Remaining limitations

- Procedural cosmetic `procedural_actions` are not executed. Standard CSS selectors, including Chromium-supported selectors, are covered.
- Generic collection is bounded to 10,000 distinct classes and IDs and bounded string sizes. Extremely large documents may require incremental collection rather than the current document scan.
- Initial cosmetic/scriptlet configuration can arrive after parser scripts in a new renderer. Custom user scripts have verified document-start registration; this does not imply all scriptlets execute before site code.
- One upstream parameterized redirect (`click2load.html`) is deliberately excluded. Trusted-only resources remain unavailable to ordinary lists. No universal extension/filter/scriptlet compatibility claim is made.
- UI-supplied bundled/cached filter readiness, asynchronous list refresh, per-site controls, and Rust packaging are parent-owned integrations.
- Linux compilation/runtime qualification is being performed by the parent. This report does not count unavailable or failing Linux runtime tests as passed.
