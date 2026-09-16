# Browser page benchmark

Recorded 2026-09-16T17:00:06.928Z; darwin arm64; 1280×800.

**Scope:** pinned CEF with built-in native blocking and a cached filter-list snapshot. CPU frame delivery excludes Rust/Vello presentation. Extra browser user scripts are not injected. First run uses a fresh isolated profile; subsequent runs use its warm cache.

| Site | Run | Load event (ms) | FCP (ms) | Scroll response median (ms) | Scroll settle median (ms) | Frame interval p95 (ms) |
|---|---:|---:|---:|---:|---:|---:|
| wikipedia | 1 (fresh) | 800.603 | 564 | 12.729 | 162.729 | 20.142 |
| wikipedia | 2 (warm) | 338.276 | 228 | 14.065 | 164.065 | 23.725 |
| wikipedia | 3 (warm) | 247.547 | 216 | 14.168 | 164.168 | 20.105 |
| hackernews | 1 (fresh) | 319.765 | 208 | 11.323 | 161.323 | 19.733 |
| hackernews | 2 (warm) | 155.637 | 140 | 20.257 | 170.257 | 20.619 |
| hackernews | 3 (warm) | 151.692 | 136 | 25.182 | 175.182 | 18.346 |
| youtube | 1 (fresh) | 9753.402 | 1100 | 9.611 | 159.611 | 18.072 |
| youtube | 2 (warm) | 1462.947 | 1408 | 10.045 | 160.045 | 18.893 |
| youtube | 3 (warm) | 1436.533 | 1380 | 6.072 | 156.072 | 18.117 |

Scroll response includes loopback CDP observation latency. Settle includes a 150 ms quiet period. Invalid/non-scrolling pages are not zero-time successes. Frame pacing may include video/other page animation. Small sample sizes do not establish general browser performance. See JSON for raw samples, errors, URLs, titles, runtime and filter hashes.
