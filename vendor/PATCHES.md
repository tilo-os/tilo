# Patches against upstream

## Exportable histories

The fork changes also live in two standalone histories, disconnected from the
qOS application history. Their root commits are pristine Cargo registry
sources; every following commit is a reviewable topic change:

- `vendor-upstream/vello-0.9` contains `vello` and `vello_encoding` together,
  because the adaptive allocator API crosses the crate boundary. Its baseline
  is crates.io `vello` 0.9.0 plus `vello_encoding` 0.9.0. The series is split
  into six dependency-ordered review commits rather than one qOS-sized patch:
  encoding budget/accounting, renderer measurement, atlas diagnostics, the
  clipped-image PTCL floor, the viewport-scaled line floor, and compatibility
  lint cleanup. The code is now ported onto current upstream `main` as three
  independently reviewable draft pull requests:
  [adaptive bump budgets and safety floors](https://github.com/linebender/vello/pull/1861),
  [atlas-drop diagnostics](https://github.com/linebender/vello/pull/1862), and
  [toolchain-compatible lint attributes](https://github.com/linebender/vello/pull/1863).
  This keeps diagnostics and compatibility cleanup out of the performance PR
  while retaining the dependency-ordered 0.9 history used by qOS.
- `vendor-upstream/winit-0.30.13` contains winit alone, based on crates.io
  `winit` 0.30.13. It retains the registry metadata and updates
  `Cargo.toml.orig` alongside Cargo's normalized manifest, making the topic
  commits straightforward to apply to an upstream source checkout.

Run `tools/vendor-history.sh verify` to check the recorded roots and tips. Run
`tools/vendor-history.sh export OUTPUT_DIRECTORY` to create portable Git
bundles (baseline plus full history) and `git format-patch` series (topic
commits only). The verifier accepts either local topic branches or the
`origin/vendor-upstream/*` remote-tracking refs present in a fresh clone, and
the script refuses to overwrite an existing output directory.

Published crate checksums used for the baselines:

- `vello-0.9.0.crate`:
  `261359dbef879f8110ef7e1c442246c838d33d3d91cb05e0ea9288d432760c9f`
- `vello_encoding-0.9.0.crate`:
  `2346f5f0d7dccb3582fcd397b4a57b43165209f1424e0d76e85dd814db164af7`
- `winit-0.30.13.crate`:
  `a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d`

The qOS build does not depend on these refs. They are review/export artifacts;
the existing portable path overrides remain authoritative until the upstream
changes are accepted and the vendoring can be removed.

## Vello changes

`vendor/vello` and `vendor/vello_encoding` are `vello` 0.9.0 and
`vello_encoding` 0.9.0 from crates.io, verbatim, except for the ten files
below. Everything else in both trees — including `Cargo.toml`, `Cargo.lock`,
the licenses and every other `.rs` file — is the unmodified crates.io release;
diff either tree against the published crate to confirm.

The patch is one thing: size the bump-allocated scratch buffers from what a
frame measurably uses instead of from upstream's fixed, worst-case constants.
See the `[patch.crates-io]` comment in the root `Cargo.toml` and the module
doc on `vendor/vello/src/bump_budget.rs` for why.

## `vendor/vello_encoding/src/config.rs`

Adds `pub struct BumpBudget` (`INITIAL`, `UPSTREAM`, `CEILING`,
`floor_for_viewport`, `observe`, `size_in_bytes`) and threads it through
`BufferSizes::new` and `RenderConfig::new`, which now take `width_in_tiles`,
`height_in_tiles` and a `&BumpBudget` and size six buffers — lines, binning,
tiles, seg_counts, segments, blend_spill, ptcl — from `budget.max(floor)`
rather than from upstream's seven hardcoded constants. Also fixes two
arithmetic faults a naive shrink would carry over:

- `ptcl` is indexed at `tile_ix * PTCL_INITIAL_ALLOC` before any bump
  allocation happens, so `n_tiles * 64` words are structurally spoken for and
  never appear in measured usage. The buffer is now sized as that structural
  term plus `budget.ptcl.max(floor.ptcl)`, not the budget alone.
- `bin_data` shares its buffer with the draw-info region, and the config
  reports `binning_size` as `bin_data - bin_data_start`; sizing it below
  `bin_data_start` underflows that subtraction on a `u32` to near four
  billion. `bin_data` is now `bin_data_start.saturating_add(...)`.

`FLOOR_PTCL_PER_TILE` is 320, not 96: the 28-scene catalogue the floors were
measured from has no *foreign* (Wayland client) surface in it, and compositing
one is `push_layer(rounded-rect clip) + draw_image + pop_layer` — a blend
layer around a sampled image costs far more per-tile `ptcl` than the vector UI
the catalogue was measured from. At 96/tile the first frame a client ever
presents overflowed `ptcl` (`failed=0x10`) and rendered with the client's
pixels silently missing, with no prior frame for `BumpBudget::observe` to have
grown from yet. See the doc comment above `FLOOR_PTCL_PER_TILE` for the
measurement this raised it from.

Own test module (7 tests): the two faults above and the `ptcl` floor gap each
have a dedicated regression test, plus growth-from-measurement, ceiling and
the headline "far less than upstream" comparison.

## `vendor/vello/src/bump_budget.rs` (new)

`BudgetTracker`: owns one renderer's `BumpBudget`, starts a non-blocking
`map_async` readback of the GPU's `BumpAllocators` after each frame, collects
it (or an already-finished one) at the top of the next, and grows the budget
to cover what was used. `VELLO_BUMP_LOG=1` logs used-vs-reserved per frame;
`VELLO_BUMP_BUDGET=upstream` pins `BumpBudget::UPSTREAM` for an A/B
comparison from one binary.

## `vendor/vello/src/lib.rs`

`Renderer` gains a `budget: bump_budget::BudgetTracker` field. Both render
paths (`render_to_texture`/`render_to_surface` and the async
`render_to_texture_async`) collect the previous frame's counters, pass
`self.budget.budget()` into `render::render_full`/`render_encoding_coarse`,
and start or await the next readback; a budget that grew clears the engine's
buffer pool so stale-size buffers are dropped rather than held for the life
of the process. The async path always reads back now (upstream gated this on
the `debug_layers` feature, when the counters were used only for debug
visualisation) — it already awaits the counters between coarse and fine, so
reading them costs nothing extra and lets it size its *own* next frame the
same way the sync path does.

## `vendor/vello/src/render.rs`

`render_full`, `render_encoding_full` and `render_encoding_coarse` take a
`&vello_encoding::BumpBudget` and a `measure: bool` (threaded through as
upstream's existing `robust` readback flag — the fork reuses that plumbing
rather than adding a second one), and hand back the bump-counter download
buffer when `measure` is set.

## `vendor/vello/src/wgpu_engine.rs`

Adds `WgpuEngine::clear_pool()`: drops every buffer the size-class pool is
holding for reuse. Called only when the budget just grew, so old size
classes stop being kept for a request that will not come again.

## `vendor/vello_encoding/src/resolve.rs`

Adds `report_dropped_image`: when the image atlas is full and an image is
dropped (dimensions zeroed, draws as nothing), prints a throttled `eprintln!`
— first occurrence always, then one in every 300 — naming the image's size
against `atlas_size()`. `eprintln!` rather than `log`/`tracing`: this crate
depends on neither, and an application built on the toolkit installs no
subscriber, so a `log::warn!` here would reach nobody.

## `vendor/vello_encoding/src/image_cache.rs`

Adds `ImageCache::atlas_size()`, the atlas's current live side length, for
`report_dropped_image` to quote — own test included.

## `vendor/vello_encoding/src/lib.rs`

Re-exports `BumpBudget` from `config`.
