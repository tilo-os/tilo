# Three-site browser benchmark — 16 September 2026

Native CEF 152 with built-in blocking, macOS ARM64, 1280×800. Three runs per site: fresh isolated profile, then two warm runs. Screenshots visually verified Wikipedia Main Page, Hacker News front page and populated YouTube nature search results. All nine latest loads returned HTTP 200 and every run produced actual scroll movement.

| Site | Fresh load | Warm loads | Scroll response median (15 trials) | Continuous CPU frame delivery |
|---|---:|---:|---:|---:|
| wikipedia | 1.96 s | 0.43 / 0.27 s | 8.9 ms | 28.5–29.0 FPS |
| hackernews | 0.67 s | 0.15 / 0.15 s | 8.8 ms | 28.0–29.5 FPS |
| youtube | 4.17 s | 1.83 / 1.26 s | 10.5 ms | 28.5–29.0 FPS |

## Interpretation

The host currently requests 30 FPS offscreen rendering. Scroll figures measure input delivery through observed document scroll events; frame timings measure completed CPU frames delivered by CEF. Neither measures Rust/Vello input-to-screen latency or full UI presentation. A 150 ms quiet period is included in the separate settle metric in runs.md. YouTube search is used because a fresh signed-out homepage showed an empty, non-scrollable feed. Video playback is not measured.

This is a small live-network sample, with concurrent development activity recorded per run. It is not a general speed ranking or a guarantee. Cache labels describe browser profiles, not flushed OS/DNS caches. Runtime, helper and filter hashes, final URLs, raw wheel samples and raw frame intervals are in results.json. Exact procedure is in scripts/browser/README-benchmark.md.

## Earlier failures and outstanding startup finding

Earlier runs are retained in earlier-timeouts.json: Wikipedia response times grew to19–38 s and HackerNews/YouTube timed out. A local follow-up measured 21.33 s for a trivial page; its NetLog showed COOKIE_PERSISTENT_STORE_LOAD lasting 21,316 ms. That result is retained in startup-delay.json. Another fresh-profile local run loaded in 215.9 ms. This intermittent cookie initialization delay remains unexplained; the successful latest run does not establish it is fixed.

A separate CEF observer re-entry error was corrected by deferring queued navigation to the next CEF UI task in the multi-tab implementation. No encryption, sandbox, or TLS protections were disabled.

## Validation

Eight Node tests pass, covering bounded framing, actual movement, invalid samples, bounded scroll workload, preserved results on reset failure, final HTTP status, raw metric reproducibility, and independent load/scroll failure reporting. Independent scoped review passed after three benchmark corrections. Native real scroll fixture and all nine site samples completed. Full browser UI acceptance is tracked separately.
