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
