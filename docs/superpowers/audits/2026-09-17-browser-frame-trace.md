# Browser frame identity trace (in progress)

This implements the receipt-to-scene portion of the browser performance plan. It is **not** an input-to-present benchmark or a completion report.

## Collection

Set `QOS_WEBVIEW_FRAME_TRACE=1` on the Rust browser process, enable `QOS_PROFILE_OUT` for CPU frame IDs, and use a UI build with `presentation-feedback` plus `QOS_PROFILE_PRESENTATION=1` for Metal feedback. The trace is separate from `QOS_WEBVIEW_PROFILE=trace`, which enables native startup diagnostics. No page URLs, content, input values or passwords are recorded by frame identity tracing.

- `qos-webview-received`: process-scoped unique `frame_id`, native `tab_id`, and host-clock bounds sampled after packet decoding. IDs distinguish separate helpers as well as tabs within the Rust process. The tab ID alone is not globally unique.
- `qos-webview-scene`: CPU frame ID and browser frame IDs actually added to that recorded scene, including cached frames. A scene with no browser content has an empty list. Stale-density frames rejected by the widget are excluded. More than 64 distinct identities marks `overflow=true`; reject that scene from measurement.
- Existing `qos-display-frame`: actual Metal drawable callback, CPU frame ID, worker identity, drawable identity and presented time.

Join by Rust process plus browser frame ID for receipt, and by CPU frame ID within that process for scene/presentation. Keep raw records. Reject absent CPU IDs, unsupported clock domains, missing receipt records, overflow, invalid/zero presentation timestamps and ambiguous presentation matches. A frame may be received but overwritten before any scene; a recorded scene may be dropped before presentation. Neither is a zero-latency sample. Repeated cached-frame scenes are legitimate; use the first valid presentation of a received frame when calculating receipt-to-display latency. Host-clock brackets provide a latency interval rather than false precision.

## Analyzer and first live check

Run `python3 scripts/browser/frame_presentation.py path/to/app.log --output analysis.json` on stderr from exactly one Rust browser process. The analyzer joins receipt IDs to recorded CPU scenes and Metal callbacks, uses the first valid presentation of a cached frame, emits latency intervals and p50/p95/p99 bounds, and lists unmatched receipts and rejected evidence. Exit status is nonzero for invalid evidence or no samples. Nine unit tests cover ordering, repeated cached frames, drops, ambiguity, invalid clocks, mixed processes and zero-based CPU/worker frame indices.

The first isolated debug window check (`target/browser-complete-ux/task9-window-trace`) received one native browser frame but recorded no scene containing it. The initial frame did not reach an accepted WebView draw at the window geometry. There were 651 Metal callbacks, 368 with positive presentation time; zero times are rejected. The analyzer produced **zero browser latency samples**. This proves the collector/analyzer refuses to fabricate a measurement when browser content is not drawn; it does not verify a successful browser-frame-to-display join. The owned process group was forcibly cleaned up and verified absent; this was not a clean shutdown test.

## Nested layout check

A suspected retained-parent cache gap was not reproduced: WebView inherits the leaf widget's always-dirty redraw contract, and Row/Column forward it. Their subtree scenes rebuild on each draw containing a WebView. The nested-layout regression checks both reuse of the WebView image without a fresh engine frame and replacement by a new frame identity. No cache behavior was changed. Future incremental-redraw changes must preserve this provenance contract.

## What remains

A later geometry-recovery replay (`task9-window-resize-fix`) produced one real receipt→scene→Metal join: browser frame 2, CPU frame 9, receipt-to-display interval 26.84404–26.84459 ms. This is a single diagnostic sample; the run also had 658 invalid presentation callbacks and one unpresented receipt. It is not a representative latency distribution or input-to-present benchmark. Complete browser-window measurements remain unverified. Connect OS input IDs to observed content responses (not merely the next unrelated animation), then to these frame identities; verify with a controlled visual fixture before running Wikipedia, Hacker News and YouTube. Preserve unsuccessful/dropped samples and measure idle behavior separately. Existing legacy input-to-next-frame counters are explicitly labelled uncorrelated and must not be reported as input-to-present latency.

## Controlled scroll response marker (not yet live-validated)

`scripts/browser/fixtures/scroll-response.html` paints a 128×4 physical-pixel corner marker only after a wheel event is followed by a changed top-level scroll position. It encodes the signed DOM wheel delta with a magic byte and check byte. Distinct, well-spaced wheel deltas identify individual inputs; overlapping pending inputs invalidate the fixture. A separate animated square deliberately produces unrelated frames. The fixture changes the workload and is a calibration test, not a production-page benchmark.

With both `QOS_WEBVIEW_FRAME_TRACE=1` and `QOS_WEBVIEW_SCROLL_MARKER=1`, receipt records include `scroll_marker_delta` decoded from those pixels. Default tracing does not inspect this marker. `scroll_presentation.py LOG --output JSON` joins uniquely sized NativeScenario wheel inputs, receipt identities, scenes and Metal presentation. NativeScenario wheel inputs are line deltas: WebView multiplies by40 and scale, narrows to f32, then CEF divides by scale/truncates; DOM deltaY reverses the sign. This mapping needs actual native calibration. The result is app-dispatch-to-scroll-marker-display, not hardware input latency. Duplicate deltas/IDs, invalid clocks, multiple tabs and pre-input markers are rejected. Raw input/receipt/display evidence remains essential.

Verification: two Rust marker tests pass (genuine initial failure saved), three JavaScript fixture tests pass, and all14 frame/scroll analyzer tests pass. Debug browser with presentation feedback builds successfully. First real-window attempt `target/browser-complete-ux/task9-scroll-marker-window` produced79 Occluded worker outcomes, no Metal callbacks and no injected inputs (scenario waits for a first presentation). Thus zero samples, no live marker proof. Group8963 was terminated/forcibly cleaned and verified absent. This run is not a clean shutdown or performance result. Investigate window/session occlusion before rerunning; do not disable the first-presentation gate or count unpresented frames.

### Review and display-state followup

CUA `getState` confirmed the Mac is locked and cannot be automatically unlocked. This explains the Occluded window run; it does not prove that all earlier Chromium/Keychain stalls had the same cause. Manual unlock was requested, with no renderer/occlusion bypass.

Independent review found two causal false positives and both are fixed: all receipts, including unpresented ones, now invalidate markers already seen before input; the fixture records scrollY at wheel dispatch and requires subsequent movement beyond it, so an older queued scroll notification cannot acknowledge a newer wheel. Both regressions were observed failing before fixes. Fifteen Python and four JavaScript tests pass; reviewer independently reran them and confirmed both findings closed. Rust decoder now uses bounded chunks rather than unchecked dynamic indices; its two tests pass. Live native marker/presentation validation remains pending unlock.

### Unlocked-desktop validation, 17 September

Fresh CUA enumeration confirmed an unlocked desktop. Isolated optimized QA38 run `target/browser-complete-ux/task9-scroll-marker-unlocked-39` produced 1,112 UI worker frames and dispatched all three wheel inputs. The controlled page never loaded: AX remained about:blank, only one native frame arrived, and no response marker yielded a sample. Of 1,109 Metal callbacks, 993 supplied zero presented timestamps; 116 supplied nonzero timestamps. The analyzer correctly reports zero scroll samples and three unanswered inputs; these are not performance results.

A second bounded run `task9-startup-unlocked-40` sampled its own native host at eight seconds. The host was waiting in SecItemCopyMatching → Keychain item decryption → SecurityServer mach_msg throughout the sample. This demonstrates that the locked desktop alone did not cause the startup stall. It does not establish the service-side reason or prove that an access prompt exists. Computer-use safety controls rejected inspection of SecurityAgent; no alternate access path was attempted. Asked the user whether a Keychain prompt is visible, with no instruction to change access controls.

Both owned process groups were terminated after the bounded runs, required forced remaining-child cleanup, and were verified absent. They do not establish clean shutdown. GUI coordination hold released. Startup reliability, native marker causality and representative window performance remain open.
