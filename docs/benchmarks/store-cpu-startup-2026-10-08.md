# Store CPU startup and layout, 2026-10-08

`apps/store/examples/startup-layout-probe.rs` exercises the actual StoreApp widgets through the existing headless sync/layout/draw runner. It separates product-font initialization from widget construction and first-frame work, then compares a retained frame with rebuilding the tree with warm caches. It creates no native window or GPU, starts no catalogue worker, and uses a controlled diagnostic author. Sample catalogue content stays in this diagnostic.

## Measured baseline

Twenty-four fresh processes ran remotely against private profiles, eight per condition, in alternating order. Every process required the product sans, serif and monospace families, a positive actual Store layout, successful relayout/recomposition and an observable profiler sample. All saw 141 font faces. Each private profile was removed afterward. Host filesystem page caches were not dropped, so this is not cold-disk or cold-boot qualification.

| Catalogue input | First frame | Retained frame | Rebuilt with warm caches |
|---|---:|---:|---:|
| Empty | 4.270 ms | 0.038 ms | 0.165 ms |
| Existing six-item screenshot fixture | 8.318 ms | 0.227 ms | 1.651 ms |
| Synthetic 48-item page from that fixture | 31.514 ms | 3.503 ms | 12.031 ms |

These are observed medians, not optimization comparisons or frame-rate claims. Font initialization medians were 37–38 ms. First-frame ranges were 4.033–5.013 ms, 7.853–9.130 ms and 30.028–32.194 ms respectively. Median child CPU/wall ratios were 0.990, 0.990 and 0.991, so native process descheduling did not dominate these samples. Host load was retained in the raw receipt rather than inferred away.

Current source caches verified fonts and system discovery once per process. CardGrid culls offscreen drawing, while layout measures the page's cards; Store caps pages at 48. The baseline demonstrates the cost of catalogue size and rebuilding without changing page size, navigation, focus, accessibility, font verification or production behavior.

## Scope and reproduction

The raw samples, warm-rebuild pass profiles and exact source/artifact hashes are in [the JSON receipt](store-cpu-startup-2026-10-08.json). Source base: `418404c4964d0bacd30f3ccb1f69c4821fc67eb0`.

Build the `store` example `startup-layout-probe` with the configured Cargo implementation in release mode, locked. Run it with no argument, `--fixture`, or `--full-page` against separate private HOME/XDG paths; do not point it at a user's profile or desktop. `--full-page` repeats the screenshot fixture with unique app identities and text, and supplies 48 items. It is synthetic input rather than a captured live catalogue.

A first frame here includes widget construction of the retained tree, layout and CPU scene recording. Warm profiler BuildLayout includes the Taffy solve. It excludes the framework app host, app worker, native surface/device creation, GPU rendering/presentation and OS services. It does not establish matched VM startup, physical GPU latency, frame rate, idle CPU or idle RAM. Guest wall-clock layout costs cannot be attributed to this native result without matching SDK/app provenance and guest CPU/phase samples. Initial-frame per-pass profiling is not available through the current harness; only the warm rebuild's passes are captured.

## Font catalog verification (2026-10-09)

A large part of the 37–38 ms above was SHA-256 over the whole catalog: 36 files, 54,891,768 bytes. Every process hashed them before its first frame. A process now hashes only a catalog it cannot prove immutable (`tilo_font_catalog::Trust`). Such a catalog is root-owned, writable by nobody else and on a read-only mount, which on the image means `/usr`'s composefs/fs-verity tree or the launcher's read-only bind of a system runtime. For that catalog, the manifest is still compared byte for byte, file sizes and faces are still checked, and no font bytes are read. A checkout, a per-user runtime and any writable catalog are still hashed in full.

These are warm-cache medians of nine runs on an Apple M5 Max. They come from `cargo run -p tilo-font-catalog --example catalog_verify_startup` and an equivalent timing of `verify_bundle` in the test profile, where `sha2` and `fontdb` build at opt-level 3. They are not a rerun on the EPYC host:

| Step | Hash every file (before) | Immutable catalog (after) |
|---|---:|---:|
| Establish the catalog's bytes | 26.3 ms | 0.48 ms |
| Verified catalog database (bytes plus face parsing and validation) | 23.0 ms | 1.1 ms |

On x86 without SHA-NI, the hash runs at about 400 MB/s, roughly 140 ms for the catalog. That cost is now gone for the image catalog, and the remaining work does not depend on the CPU's SHA support. Host font discovery (`load_system_fonts`) is separate. It took 6.6 ms on this Mac and was not changed.
