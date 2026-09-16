# Three-site browser benchmark — 16 September 2026

Native CEF 152 with built-in blocking, macOS ARM64, 1280×800. Three runs per site: fresh isolated profile, then two warm runs. Screenshots visually verified Wikipedia Main Page, Hacker News front page and populated YouTube nature search results. All nine latest loads returned HTTP 200 and every run produced actual scroll movement.

| Site | Fresh load / first content | Warm loads | Scroll response median (15 trials) | Continuous CPU frame delivery |
|---|---:|---:|---:|---:|
| wikipedia | 0.80 / 0.56 s | 0.34 / 0.25 s | 14.1 ms | 59.0–60.1 FPS |
| hackernews | 0.32 / 0.21 s | 0.16 / 0.15 s | 20.3 ms | 59.0–60.0 FPS |
| youtube | 9.75 / 1.10 s | 1.46 / 1.44 s | 8.9 ms | 59.2–60.0 FPS |

## Interpretation

The host now requests 60 FPS offscreen rendering. Scroll figures measure input delivery through observed document scroll events; frame timings measure completed CPU frames delivered by CEF. Neither measures Rust/Vello input-to-screen latency or full UI presentation. A 150 ms quiet period is included in the separate settle metric in runs.md. YouTube search is used because a fresh signed-out homepage showed an empty, non-scrollable feed. Video playback is not measured.

This is a small live-network sample, with concurrent development activity recorded per run. It is not a general speed ranking or a guarantee. Cache labels describe browser profiles, not flushed OS/DNS caches. Runtime, helper and filter hashes, final URLs, raw wheel samples and raw frame intervals are in results.json. Exact procedure is in scripts/browser/README-benchmark.md.

The controlled local comparison in frame-rate-comparison.md supports the frame-rate increase: around 30 FPS became 59–60 FPS at identical input cadence. Earlier site measurements remain in results-30fps.json and runs-30fps.md. Live-network loading varied between runs; the frame-rate change does not establish improved network loading speed.

## Earlier failures and outstanding startup finding

Earlier runs are retained in earlier-timeouts.json: Wikipedia response times grew to19–38 s and HackerNews/YouTube timed out. A local follow-up measured 21.33 s for a trivial page; its NetLog showed COOKIE_PERSISTENT_STORE_LOAD lasting 21,316 ms. That result is retained in startup-delay.json. Another fresh-profile local run loaded in 215.9 ms. This intermittent cookie initialization delay remains unexplained; the successful latest run does not establish it is fixed.

A separate CEF observer re-entry error was corrected by deferring queued navigation to the next CEF UI task in the multi-tab implementation. No encryption, sandbox, or TLS protections were disabled.

## Validation

Eight Node tests pass, covering bounded framing, actual movement, invalid samples, bounded scroll workload, preserved results on reset failure, final HTTP status, raw metric reproducibility, and independent load/scroll failure reporting. Independent scoped review passed after three benchmark corrections. Native real scroll fixture and all nine site samples completed. Full browser UI acceptance is tracked separately.
