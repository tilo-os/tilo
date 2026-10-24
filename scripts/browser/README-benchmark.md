# Browser load and scroll benchmark

Run from the repository root with Node 22 or newer and the built CEF helper:

```sh
node --test scripts/browser/benchmark-pages.test.mjs
node scripts/browser/benchmark-pages.mjs --runs 3 --output target/browser-page-benchmark
```

The output directory must not already contain `results.json`. Optional `--host` selects a packaged helper; `--filter-cache` selects the cached filter-list directory. `--sites` accepts a JSON array of `{ "name": "fixture", "url": "http://127.0.0.1:8000/" }` for repeatable local diagnostics.

## Workloads

- Wikipedia: English Main Page.
- Hacker News: front page.
- YouTube: search results for `nature`. A fresh signed-out YouTube homepage can have no feed and no vertical scroll range, so it cannot provide a valid scroll measurement. This workload measures browsing search results, not video playback.

Each site gets an isolated temporary profile. Its first run uses that fresh profile; later runs reuse its cache. This does not flush OS, DNS, or network caches. The harness uses built-in native blocking and a snapshot of the available seven standard cached filter lists; hashes and missing lists are visible in JSON. Extra browser user scripts are not injected. Sandbox and TLS settings remain enabled.

## What the numbers mean

- **Load event:** elapsed host-command time until the main document load event, including IPC and navigation. HTTP success and the final document identity are checked. This is not the time until every asynchronous application request has finished.
- **First contentful paint (FCP):** Chromium's navigation-relative first visible text/image paint, in milliseconds.
- **Scroll response:** five separate wheel inputs; elapsed time until a real top-level scroll event reaches the loopback debugging connection. It includes observation latency and is not input-to-screen latency.
- **Scroll settle:** the last scroll event plus a fixed 150 ms quiet period. That quiet period is included in the result, not presented as rendering work.
- **Continuous scroll:** 30 downward and 30 upward wheel steps over roughly one second (16 ms between inputs). Distances adapt to the document's scroll range to avoid measuring idle frames at the page bottom.
- **Frame delivery:** intervals between complete CPU frames received from CEF. The current host is configured for **60 FPS** offscreen rendering. These figures exclude Rust/Vello presentation and can include unrelated page animation.

The harness saves raw results, environment/runtime/filter hashes, screenshots, and helper logs. Each pacing result includes `frameTimestampsMs` (Node process monotonic milliseconds) and `rawFrameIntervalsMs`, so its interval summary and delivered FPS can be recomputed. A failed warm reset stops that site, preserves earlier samples, and records the failed reset in JSON and Markdown. Invalid loads, non-scrolling pages, consent redirects, and timeouts are failures or unavailable metrics, never zero-time successes. Small samples and live websites are unsuitable for broad performance claims. Compare repeat runs on the same machine and configuration; per-run system load is recorded because concurrent development builds can affect results.

## Native typing and retained-page regressions

```sh
python3 -m unittest discover -s scripts/browser -p 'test_benchmark_input.py'
python3 scripts/browser/benchmark-input.py target/chromium/tilo-chromium-host \
  --output target/input-1280.json
python3 scripts/browser/benchmark-input.py target/chromium/tilo-chromium-host \
  --width 3840 --height 2160 --output target/input-3840.json
```

Each native key press/release must produce a complete CPU frame whose pixel
marker identifies the input's actual text length. An unrelated frame or a missing
response cannot count as success. Reports retain every latency sample, the host
executable digest and load per CPU before and after. Linux runs also record
memory pressure and reject full-stall averages above 1%. A busy or swapping host
invalidates the run rather than producing a misleading comparison; use the same machine,
viewport and load limit for both revisions. Outputs are never overwritten.

These numbers exclude Rust/Vello and display presentation. They cannot establish
parity with a native Chromium window. Run the presentation diagnostics below as
well before making end-to-end responsiveness claims.

The Browser unit test `hosted_address_edits_retain_the_page_and_its_viewport`
checks that host address-bar edits retain the WebView rather than resizing a new
one per character, while opening Find still changes the composition. The host's
protocol tests pin the single-copy frame packet to the existing wire format,
including tab routing; shared-host frame tests verify forwarding without conversion.
`t outputs chromium-host` builds and tests the engine remotely and retrieves its
complete runnable directory for a local browser launch.

## Actual browser-window presentation diagnostics

The engine benchmark above does not measure when the Rust browser window displays
an input response. Two separate analyzers consume opt-in logs from a browser built
with `ui/presentation-feedback`:

```sh
python3 scripts/browser/frame_presentation.py path/to/browser.log --output frame-presentation.json
python3 scripts/browser/scroll_presentation.py path/to/browser.log --output scroll-presentation.json
```

`frame_presentation.py` joins Chromium frame receipt, the Rust scene using that
frame, and Metal presentation by frame identity. Its result is receipt-to-display,
not input latency. Retain the raw log alongside the JSON result.

`scroll_presentation.py` additionally requires the controlled
`fixtures/scroll-response.html` page, native scenario wheel-dispatch records, and
frame tracing with `TILO_WEBVIEW_FRAME_TRACE` and `TILO_WEBVIEW_SCROLL_MARKER` enabled.
The fixture changes a pixel marker only after the requested document scroll;
unrelated animation alone cannot establish a response. The analyzer requires a
single tab, 100% page zoom, and distinct wheel deltas, rejects markers already observed before
input, and reports unanswered inputs explicitly. This measures synthetic
application input to display, not physical mouse or trackpad latency.

Zero or invalid presentation timestamps, missing frames, ambiguous input markers,
and unanswered inputs are failed or incomplete evidence. Do not replace them
with frame-delivery FPS or zero-latency samples. A passing controlled fixture does
not establish Wikipedia, Hacker News, or YouTube performance: the three actual
page workloads still require their own browser-window measurements.

Offline checks for the measurement logic:

```sh
python3 -m unittest discover -s scripts/browser -p 'test_*presentation.py'
node --test scripts/browser/benchmark-pages.test.mjs scripts/browser/scroll-response.test.mjs
```
