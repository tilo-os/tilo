# Desktop bundle size and open time

`build.sh OUT --bundle desktop --profile release` compiles with Cargo's
`wasm-release` profile (opt-level 3, fat LTO, one codegen unit, `panic =
"abort"`, stripped, no debug info) and runs `wasm-opt` with the arguments in
`tools/web/wasm-opt-flags`, the one list every web module of this repository
is optimized with. The development bundle is 325 MB.

## Budget

`tools/web/wasm-size-budget/` holds the module
(`pkg/tilo_browser_runtime_bg.wasm`) to a budget: a measurement plus 1%
headroom, rounded up to 64 KiB. `tools/web/wasm_size_ratchet.py measure
desktop OUT` fails when it grows past the budget and asks for a lower budget
when it shrinks well below it, and the app-sdk workflow refuses a budget
larger than the base revision's beyond what the change's new raise files
(`raises/desktop/<change>.toml`, one per change) add. The
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

## Where the code goes

The 40-app desktop's start module on 2026-11-07 — 28,637,262 B raw, 7,106,605
B brotli -q 11, 8,777,187 B as the page's first load receives it — is
26,518,549 B of function bodies in 33,849 functions. By the crate that owns
each function:

| crate | bytes | share |
|---|---|---|
| tilo_ui | 4,280,341 | 16.1% |
| core | 2,968,204 | 11.2% |
| serde_core | 1,449,409 | 5.5% |
| alloc | 1,263,762 | 4.8% |
| tilo_peer_state | 915,260 | 3.5% |
| automerge | 912,562 | 3.4% |
| serde_json | 878,620 | 3.3% |
| tilo_browser_runtime | 779,124 | 2.9% |
| tilo_desktop | 670,527 | 2.5% |
| moq_net | 558,982 | 2.1% |
| naga | 515,457 | 1.9% |
| read_fonts | 512,636 | 1.9% |
| tilo_desktop_model | 490,220 | 1.8% |
| skrifa | 484,001 | 1.8% |

What repeats across that code is generic instantiations, one copy per call
site or per type:

| family | functions | bytes |
|---|---|---|
| `serde_core::de` (per deserialized type) | 1,394 | 1,461,060 |
| `ptr::drop_glue` (per owning crate and type) | 2,662 | 1,233,387 |
| std's stable sorts (per element type and comparator) | 558 | 993,541 |
| `BTreeMap`/`BTreeSet` internals (per key and value type) | 528 | 715,061 |
| `core::fmt` (per formatted type) | 1,398 | 179,236 |

Most of it is inherent: a `Deserialize` impl or a drop glue is written per type
and nothing outside the standard library can share it, and the standard
library's B-tree is generic over its key and value. Sorts are the exception —
each call site compiled the whole driftsort quicksort, its small-sort helpers
and its merge — and the toolkit's own sorts now go through
`tilo_sort::CompactSort`'s one merge sort instead, which took about half a
megabyte of the copies out. The layout solver was the other exception: every
closure handed to `compute_layout_with_measure` compiled taffy's flexbox, grid
and block solvers again, so toolkit code solves through `layout::solve_layout`
alone. No crate in the module is compiled twice: each crate's hash appears
once. What is left is per-type code the shell genuinely runs before its first
frame, so the next cut is by feature family (moving a surface a visit does not
need behind the lazy split points) rather than by deduplication.

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
