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

## What remains

A successful live browser-frame-to-display join and actual browser-window measurements remain unverified. Connect OS input IDs to observed content responses (not merely the next unrelated animation), then to these frame identities; verify with a controlled visual fixture before running Wikipedia, Hacker News and YouTube. Preserve unsuccessful/dropped samples and measure idle behavior separately. Existing legacy input-to-next-frame counters are explicitly labelled uncorrelated and must not be reported as input-to-present latency.
