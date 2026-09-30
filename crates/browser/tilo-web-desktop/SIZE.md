# Desktop bundle size and open time

`build.sh OUT --bundle desktop --profile release` compiles with Cargo's
`wasm-release` profile (opt-level 3, fat LTO, one codegen unit, `panic =
"abort"`, stripped, no debug info) and runs `wasm-opt -Os`. The development
bundle is 325 MB.

## Budget

`tools/web/wasm-size-budget.toml` holds the module
(`pkg/tilo_browser_runtime_bg.wasm`) to its measured size;
`tools/web/wasm_size_ratchet.py measure desktop OUT` fails when it grows past
the budget and asks for a lower budget when it shrinks by more than 1%, and
the app-sdk workflow refuses a budget larger than the base revision's. The
transfer sizes below were measured once with gzip -9 and brotli -q 11.

| 17 apps, 2026-09-26 | raw | gzip -9 | brotli -q 11 |
|---|---|---|---|
| wasm | 52.6 MB | 19.5 MB | 13.3 MB |
| first visit: wasm, glue, modules, apps.json, startup fonts | 57.4 MB | 21.6 MB | 15.0 MB |
| on request: `data/` (the Typst editor's fonts) | 9.8 MB | 6.4 MB | 5.1 MB |

| 31 apps (SDK integration), 2026-09-26 | raw | gzip -9 | brotli -q 11 |
|---|---|---|---|
| wasm | 67.9 MB | 24.8 MB | 16.5 MB |
| first visit | 72.7 MB | 26.9 MB | 18.2 MB |

The 14 apps the services, net and media waves made web-capable (network,
media, PDF, Photos, Tube) add 15.3 MB raw and 3.2 MB brotli to the module.

Before the Typst editor's fonts moved to `data/`, the first visit was 67.0
MB raw, 28.0 MB gzip and 19.0 MB brotli. The editor now asks for them when
it opens (`ui::host_data`); the page fetches and verifies them against
`data.json`.

The module is about 35 MiB of code and 15 MiB of data. What remains of the
data is mostly Typst's too: function documentation, the CSL styles and
locales behind bibliographies (3.2 MB, 0.4 MB under brotli: moving them
would mean forking hayagriva, whose `archive` embeds them and which
typst-library reads directly), hyphenation patterns; then the time-zone
database and Vello's shaders.

## Profile, measured

Same source, headless Chromium on WebGPU (Apple M-series), one page, fresh
browser context. "First open" is the 17 apps opened once each on a fresh
desktop; "again" opens each a second time with the first 17 still open. Open
time is from dispatching the desktop's `launch` command to the first frame
presented after it (`scripts/open-times.mjs`).

| variant | raw | brotli | first open p50 / max | again p50 / max |
|---|---|---|---|---|
| opt-level 3 | 66.3 MB | 17.1 MB | 30.3 / 69.7 ms | 46.4 / 76.0 ms |
| opt-level 3, wasm-opt -O3 | 62.4 MB | | 31.3 / 73.1 ms | 45.4 / 75.9 ms |
| opt-level 3, wasm-opt -Os | 62.3 MB | 17.3 MB | 30.3 / 73.8 ms | 45.7 / 76.0 ms |
| opt-level s | 58.8 MB | 15.7 MB | 34.5 / 100.8 ms | 67.7 / 110.9 ms |
| opt-level s, wasm-opt -Os | 54.7 MB | 16.3 MB | 34.1 / 103.6 ms | 63.9 / 104.8 ms |

opt-level `s` saves 8% of the transfer and costs 40–50% more per frame once
windows accumulate; opt-level 3 stays. wasm-opt after LLVM changes neither
open time nor transfer size measurably (it compresses slightly worse under
brotli) but takes 6% off the module the engine compiles and keeps in memory.
wasm32-unknown-unknown has no stable unwinding, so `panic = "abort"`: the
runtime's panic hook reports the message and the module traps.

## Open time

31 apps, release bundle, each app in a fresh headless Chromium (one page,
fresh browser context, launched from the desktop 600 ms after it presents):
first opens took 8–44 ms (p50 17 ms, p90 31 ms; slowest video-editor, notes,
word-processor, tube). The desktop presented 185–375 ms after navigation
(p50 202 ms). A fresh browser per app, because the media wave saw one browser
opening every app run out of memory.


The profile and open-time measurements below predate moving the fonts;
opening the Typst editor now skips parsing them (first open 45 ms instead of
80 ms on the same loaded machine), and the rest is unchanged.

Release bundle, two pages side by side, each opening all 17 apps twice:
every open took 13–77 ms (p50 33 ms). Frame cost grows by about 1.3 ms per
open window, so an app opened into a desktop of 18 windows takes 37–71 ms.
The desktop presents 325–490 ms after navigation with an empty cache on
localhost; a restored desktop of 35 windows, 450–570 ms, because only the
focused window is built before the first frame and the rest follow within
8 ms per frame.
