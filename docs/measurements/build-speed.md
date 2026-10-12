# Build speed and the compilation cache

## What does not make builds faster here

Measurements from `ui`'s test binary:

| change | effect |
|---|---|
| mold instead of GNU ld | none (40 s vs 41 s, and 61 s vs 61 s) |
| `-C split-debuginfo=unpacked` | binary 697 -> 507 MB, build 14% **slower**; see [debug info in test binaries](#debug-info-in-test-binaries) |
| `-C debuginfo=0` | binary 665 -> 475 MB, build unchanged |
| nightly `--jobs-frontend=4` / `=8` | +13% / 0% |
| nightly cranelift backend | 23% slower |

Compilation dominates: `-Z time-passes` puts type checking at 14% of a debug
build and LLVM at 29%, the rest rustc lowering MIR to IR.

Run-to-run noise on a busy machine is ±20%, which is larger than most of the
effects above. Measure on an idle machine.

Use sccache to share compilation results across worktrees; target directories
cannot be shared. A measured `ui` build in a fresh worktree took 126 s at 77%
cache hits, compared with ~562 s cold.

Changing `build.rustflags` invalidates cached compilations across worktrees.
Batch flag changes and avoid them during verification runs.

## Sizing the compilation cache

116 members over ~1,480 external crates. One full debug+test build is ~2,200
cacheable units, a few GB compressed (measured: 105,000 objects in 100 GB, mean
0.97 MB, median 0.22 MB).

Cache dependencies between lockfile changes. Changes to a shared crate such as
`ui` rebuild the compositor and dependent apps. At roughly 3–5 GB per full set,
200 GB holds about forty versions.

Prefer cache over target directories when trading disk: a cached object serves
every worktree and survives `cargo clean`, a target directory serves one and
does not.

The compiler is part of the cache key, so bumping `rust-toolchain.toml` costs one
fully cold rebuild.

## Debug info in test binaries

Each test binary links every crate it depends on, line tables included, so a
workspace test build holds ~460 copies of overlapping debug info. Even at
`debug = "line-tables-only"`, about 75% of shell's `it` binary was DWARF
(`.debug_str` 557 MB, `.debug_line` 229 MB, `.debug_info` 121 MB against 219 MB
of `.text`). The linker therefore compresses debug sections with zlib
(`.cargo/config.toml`). Measured on `cargo test -p shell --tests --no-run`:

| variant | `target/debug` | test binaries | `it` | file:line backtraces | sccache |
|---|---|---|---|---|---|
| uncompressed | 22 GB | 10.3 GB | 1730 MB | yes | 100% cacheable |
| `split-debuginfo = "unpacked"` | 19 GB | 6.5 GB | 1095 MB | yes | hits drop the `.dwo` files |
| zlib (current) | 15 GB | 5.0 GB | 897 MB | yes | 100% hits on a rebuild |

`zstd` compresses as well but std's backtrace symbolizer only reads zlib:
with zstd, panics lose file:line. The cost is at the point of use: linking
`it` with mold took 2.7 s against 1.8-3.3 s uncompressed (within noise), and the first backtrace in a process
decompresses the sections (1.8 s -> 4.4 s for a panicking `it` test).

`split-debuginfo = "unpacked"` leaves line tables, ranges and aranges in each
binary and moves the rest into per-codegen-unit `.dwo` files under
`target/debug/deps`. sccache 0.17 caches those compilations but does not
store the `.dwo` files: after a cached rebuild, 4,361 of the 4,409 `.dwo`
files `it` referenced were missing, and a hit replayed from another worktree
points at that worktree's paths. `packed` would write a `.dwp` per binary,
duplicating the data again.

