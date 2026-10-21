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


## OS images on t

The OS image is built as a chain of cached t checks rather than one image
build (`os/image/layers.toml`, written into `t.toml` by
`tools/ci/os/os_toml.py`). The workspace compiles outside any image build
(`os-compile-<arch>`, `os/image/compile.sh`) with t's compiler cache; each
layer action builds its pieces of `os/image/` over the layer below and keeps
only its own layer blobs (`tools/ci/os/oci_layers.py take`); `os-image-<arch>`
composes the blobs into an OCI layout without rebuilding anything. A change
reruns only the checks whose inputs it changes, and the dev and board images
are layered over the OS image's layout.

Per-check times (from each check's `t: used … in` line), first after an input
change to every layer ("cold"), then after a comment change in one shell crate
("warm"), on a shared and busy cluster:

| check | x86_64 cold | x86_64 warm | aarch64 cold | aarch64 warm |
|---|---|---|---|---|
| image-rpms | 17m22s | cached | 4m20s | cached |
| builder | 11m12s | cached | 3m53s | cached |
| tools | 2m59s | cached | 7m08s | cached |
| compile | 9m30s | 10m39s | 6m55s | 9m39s |
| compile-dev | 4m15s | 9m28s | 3m35s | not recorded |
| layer packages | 16m30s | cached | 5m04s | cached |
| layer kernel | 3m58s | cached | 2m15s | cached |
| layer system | 5m33s | cached | 2m21s | cached |
| layer trust | 2m18s | cached | 2m16s | cached |
| layer binaries, rootfs, boot | 20m31s | 11m00s | 10m17s | 11m09s |
| compose | 15s | not recorded | 33s | not recorded |
| dev image | 10m29s | 6m35s | 4m58s | 4m40s |
| board image (Orange Pi 3B) | | | 7m48s | 7m27s |

The binaries, rootfs and boot pieces were then merged into one `image` layer
action, which removes two loads and two pushes of the multi-gigabyte
intermediate images (9m09s on aarch64).

A warm OS image is the compile plus the `image` layer plus compose: about 21
minutes on x86_64; the dev image adds 6–7 minutes, the board image 7–8.

A second warm run, after the same kind of change on a later tree, landed on
build nodes that had just replaced the ones the cold run used:

| check | x86_64 | aarch64 |
|---|---|---|
| compile | 22m12s (4% of compiles from the cache) | 15m35s (6%) |
| compile-dev | 17m42s | 11m33s |
| image layer | 33m49s (build 19m39s at 0.6 cores, push 8m22s) | 14m44s |
| compose | 0.3 s | 0.5 s |
| dev image | no x86_64 node left to run it | 5m05s |
| board image (Orange Pi 3B) | | 6m34s |

What remains above the 20-minute target:

- The compiler cache and the build snapshots live on each node, so a node
  that rotates out takes them along and the next compile is cold again (4–6%
  hits above, 75–88% on a node that kept its cache). Without a warm snapshot
  the compile rebuilds the changed crate's dependents at fat LTO (10 minutes
  with a warm cache); with a snapshot the same compile took 40 s to 1m43s.
- Every layer action pays fixed costs: starting the assembler (15–35 s),
  loading the layers below (30–100 s), verifying its inputs (15–30 s) and
  recompressing the image it pushes (1–3 minutes).
- The board image pushes two complete zstd archives.
- Cluster contention: the same checks took up to ten times as long in runs
  that queued behind other work.
