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

## Nested application startup regression

The packaged native executable initially exited before ready with SIGTRAP inside
`cef_initialize`. The new `nested_bundle.py` reproduced that crash. CEF defaults
`main_bundle_path` and `framework_dir_path` to the outer application, but the
outer Rust `Browser.app` does not contain CEF directly in its Frameworks folder.
The host now resolves both paths from its own canonical executable, selecting
`Browser Engine.app` and the framework inside it. No sandbox or TLS setting changed.

After rebuilding and the parent's package copy:

- `nested_bundle.py <packaged-host>`: **1 passed in 2.700s**, verifying ready,
  local HTTP navigation/title, 1280×800 frame delivery and clean exit.
- `smoke.py <packaged-host>`: **PASS**, final `https://podesta.ai/`, HTTP 200,
  title `Podesta AI — The operating system for the AI age`, nonempty BGRA frame,
  closed event, exit 0.
- `smoke.py <standalone-host>`: **PASS** with the same HTTPS acceptance.
- Rebuilt standalone bundle and parent-reported packaged strict signatures pass.

CEF stderr still reports observer-list reentrancy diagnostics, a process signature
validation-category diagnostic with ad hoc signing, and a nested helper Resources
sandbox-extension warning. Those diagnostics did not prevent the verified renderer,
HTTPS navigation or shutdown; they are not silently counted as resolved here.

## Review round 1: request initiators, CSP cosmetics, Linux onboarding

The review fixture exposed two native defects, reproduced in permanent tests:

- Cross-origin iframe navigation used the target frame URL as the source. The
  third-party/domain rule requests incorrectly reached the server and the inverse
  first-party rule incorrectly blocked a cross-site frame. Requests now prefer
  the CEF request initiator; subdocument fallback uses the embedding parent frame.
  Tests cover cross-site/same-site frames and source-domain restrictions.
- An inserted `<style>` element was refused by `style-src 'none'`, leaving all
  cosmetic fixtures visible. Cosmetics now use a constructed CSSStyleSheet
  adopted by the document. This does not relax CSP. Tests verify initial/dynamic
  hiding, disable/re-enable, preservation of the page's own adopted stylesheet,
  and continued blocking of page-authored inline CSS.

The parent separately isolated Linux startup hanging inside CefInitialize to
Chromium's first-run flow. The original Linux host passed HTTPS smoke with only
`--no-first-run` added. The host now adds that switch for the primary process:
qOS owns onboarding and cannot present Chromium's separate first-run UI. No
password-store, sandbox, or TLS override was added.

Validation after these changes: macOS native build successful; all **11 blocker
fixtures passed in 20.219s**; native protocol ctest passed; strict macOS bundle
signature/symlink verification passed. Existing four native regressions passed in 7.796s. Parent is validating the Linux build/runtime.

The new CSP fixture deliberately observes computed styles from its own permitted
script. During its creation, a separate limitation was discovered: late custom
script updates can be blocked by CSP due to the wrapper's indirect eval, despite
working at document start. A separate fix and regression are required next.

## Follow-up: late custom scripts under script CSP

The new late-update test reproduced the remaining CSP defect: after loading a
page with `script-src 'self' 'unsafe-inline'`, the configured counter stayed at 0
instead of 1 because the wrapper called page `eval()`. The shared document-start/
live-update source now includes the trusted script bodies directly in individual
try blocks. It retains the direct HTTP(S) guard and per-document configuration
marker, without weakening the page policy or introducing native page bindings.

The regression verifies a global `var` declaration still reaches the page global,
identical configuration runs once, changed configuration runs once more, and the
page's own eval remains prohibited by CSP. Existing document-start, reload,
cross-site and internal-scheme boundary regressions exercise the same source.

Validation: native build successful; all **12 blocker fixtures passed in
29.109s**; all **4 existing native regressions passed in 21.448s**; macOS
bundle symlinks and strict recursive signature verification passed.
