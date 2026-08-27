# Patches against upstream

`vendor/vello` and `vendor/vello_encoding` are `vello` 0.9.0 and
`vello_encoding` 0.9.0 from crates.io, verbatim, except for the eight files
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

Own test module (6 tests): the two faults above each have a dedicated
regression test, plus growth-from-measurement, ceiling and the headline
"far less than upstream" comparison.

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
