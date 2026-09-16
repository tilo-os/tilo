# Browser page benchmark

Recorded 2026-09-16T16:29:21.231Z; darwin arm64; 1280×800.

**Scope:** pinned CEF with built-in native blocking and a cached filter-list snapshot. CPU frame delivery excludes Rust/Vello presentation. Extra browser user scripts are not injected. First run uses a fresh isolated profile; subsequent runs use its warm cache.

| Site | Run | Load event (ms) | FCP (ms) | Scroll response median (ms) | Scroll settle median (ms) | Frame interval p95 (ms) |
|---|---:|---:|---:|---:|---:|---:|
| wikipedia | 1 (fresh) | 1964.304 | 1632 | 26.812 | 176.812 | 46.637 |
| wikipedia | 2 (warm) | 427.331 | 272 | 29.521 | 179.521 | 45.449 |
| wikipedia | 3 (warm) | 274.56 | 236 | 8.788 | 158.788 | 43.891 |
| hackernews | 1 (fresh) | 672.993 | 264 | 8.761 | 158.761 | 66.506 |
| hackernews | 2 (warm) | 154.513 | 136 | 11.268 | 161.268 | 66.628 |
| hackernews | 3 (warm) | 147.457 | 132 | 7.938 | 157.938 | 35.581 |
| youtube | 1 (fresh) | 4167.4 | 1316 | 7.33 | 157.33 | 44.904 |
| youtube | 2 (warm) | 1834.936 | 1104 | 13.121 | 163.121 | 49.127 |
| youtube | 3 (warm) | 1264.469 | 944 | 9.87 | 159.87 | 46.239 |

Scroll response includes loopback CDP observation latency. Settle includes a 150 ms quiet period. Invalid/non-scrolling pages are not zero-time successes. Frame pacing may include video/other page animation. Small sample sizes do not establish general browser performance. See JSON for raw samples, errors, URLs, titles, runtime and filter hashes.
