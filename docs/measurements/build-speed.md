# Build speed and the compilation cache

## What does not make builds faster here

Measurements from `ui`'s test binary:

| change | effect |
|---|---|
| mold instead of GNU ld | none (40 s vs 41 s, and 61 s vs 61 s) |
| `-C split-debuginfo=unpacked` | binary 697 -> 507 MB, build 14% **slower** |
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
