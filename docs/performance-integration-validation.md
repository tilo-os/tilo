# Performance integration validation — 2026-09-06

This records correctness checks, **not new performance measurements**. The
integration combines performance snapshot `136613c6a6a47746d9ef91db91d4580c71dfab23`
with main `6b94031a1dcc6d23820802a7858171fc5203745b` and the merge corrections
in this branch. The historical timing matrix remains in Git history at
`f780ae5c0`, in `docs/benchmarks/2026-09-06-current-main/RESULTS.md`. It uses an
earlier main revision; its numbers must not be relabelled as this merge.

## Build and focused checks

The final merged source was compiled in a separate worktree/target directory:

```sh
RUSTC_WRAPPER=/usr/bin/env CARGO_INCREMENTAL=0 \
cargo test -p ui -p shell -p vello --lib \
  --example ui-perf --example redraw_bench --locked --no-run
```

The rebuilt binaries were run single-threaded per suite. These are targeted
tests, not the entire workspace test suite, and they use the test profile,
not release benchmark binaries.

| Suite / filter | Passed | Ignored |
| --- | ---: | ---: |
| UI semantic resource cache and lazy fallback | 21 | 0 |
| UI animation pump | 2 | 0 |
| UI audience monitor identity and fullscreen selection | 9 | 0 |
| UI native damage versus full-render pixel checks | 2 | 0 |
| UI action palette | 12 | 0 |
| UI performance-harness unit tests | 8 | 0 |
| Shell native compositor, including retained audience-removal regression | 43 | 0 |
| Shell audience routing | 8 | 0 |
| Shell cursor interpolation/idle projection | 13 | 0 |
| Shell DRM backend | 221 | 0 |
| Shell redraw harness, including native 3840×2160 pixel oracle | 54 | 7 |
| Vendored Vello renderer | 118 | 2 |

Total: 511 passed, zero failed, nine ignored. Ignored tests are not counted as
passing validation.

The audience integration drains bounded render-completion feedback, keeps
scene acquisition nonblocking when no scene is ready, balances scene/damage
buffer recycling, and uses full-frame invalidation for appended audience
content. Removal invalidation remains pending across deferred ticks until
submission. The queue paths also received structural/manual review; this is
not a physical multi-monitor end-to-end test.

Formatting and non-archive whitespace checks passed. All local links in the
[performance flag reference](performance-flags.md) resolve. A source inventory
checked renderer, benchmark, startup, DMA-BUF, and loop diagnostic controls.
Raw benchmark logs were not reformatted; they remain recoverable from Git
history after the archive directory's removal from this branch.

## Limits and outstanding gates

The final UI/Vello strict check passed:

```sh
cargo clippy -p ui -p vello --lib --example ui-perf --locked --no-deps -- -D warnings
```

The combined strict Clippy command for UI, shell, Vello, and both examples
does **not** pass: it reports 171 shell errors. Every diagnostic's primary
source snippet was found unchanged in main `6b94031a1`; no new primary source
snippet was reported. This source comparison is not a two-build diagnostic
baseline and does not establish a green CI lint gate. Unrelated shell lint
cleanup was deliberately excluded from this performance-only integration.

Before push, `cargo deny`, `cargo machete`, and `fgj` were unavailable locally;
remote CI therefore remains a separate gate. Do not merge to main based only
on these local checks.

The diff relative to the integrated main revision excludes boot/security,
application, filesystem, and CI-workflow changes. The 17 unrelated local
boot/security files in the original checkout were verified byte-identical.

There is no fresh release A/B run for this merged revision, no direct physical
VRAM measurement, and no physical 4K60 presentation result on a 2010 laptop.
The constant-60-fps objective remains unmet. See the flag reference for the
configured x86 CPU floor and for the distinction between CPU writes, GPU
readback payload, known GPU allocations, and physical presentation.
