# Patches against upstream

## Cast transport

`fcast-sender-sdk` 0.3.0 is the checksum-verified MIT registry release, moved to
edition 2024. Its local patch verifies TLS handshake signatures with Rustls/ring
and authenticates DeviceAuth before control or media messages on that connection.
Dedicated Cast/Eureka identity and Cast CRL anchors are private protocol constants;
Open Screen's BSD-3-Clause license and immutable revision/DER pins are retained.
Fresh nonces, SHA-256 proofs, validated paths, screen capability, current signed
revocation and bounded negotiation are required. Expiry tears down the session.
No machine trust setting or persistent access grant changes. See its
`PROVENANCE.md`, production-source TLS/authentication regression tests and
`docs/architecture/continuity-interop.md`.

## Spotify dependencies

`librespot-core` 0.8.0 and `hyper-proxy2` 0.1.0 are checksum-verified MIT
registry releases. The former moves to quick-xml 0.41 and invalidates a
non-Premium session without exiting Music. The latter reuses the workspace's
current Rustls/ring dependencies and Tilo's fixed-path native-root loader,
removing the old certificate-validation and OpenSSL-probing chain. Their
upstream TLS defaults remain untouched; Music explicitly selects Rustls.
Both directories contain `PROVENANCE.md`, licences and focused regression
tests. No insecure verifier, alternative trust loader or policy exception is
introduced.

## Exportable histories

The fork changes also live in two standalone histories, disconnected from the
Tilo application history. Their root commits are pristine Cargo registry
sources; every following commit is a reviewable topic change:

- `vendor-upstream/vello-0.9` contains `vello` and `vello_encoding` together,
  because the adaptive allocator API crosses the crate boundary. Its baseline
  is crates.io `vello` 0.9.0 plus `vello_encoding` 0.9.0. The series is split
  into eight dependency-ordered review commits rather than one Tilo-sized patch:
  encoding budget/accounting, renderer measurement, atlas diagnostics, the
  clipped-image PTCL floor, the viewport-scaled line floor, and compatibility
  lint cleanup, separation of budget readback from debug-buffer capture, and
  the upstream lint-format cleanup found by the feature-matrix CI. Later
  commits sync the maintained source tree and add an API switch for canonical
  segment order, so exact-pixel oracles render repeatably without
  `VELLO_CANONICAL_SEGMENT_ORDER`.
  The code is now ported onto current upstream `main` as three
  independently reviewable draft pull requests:
  [adaptive bump budgets and safety floors](https://github.com/linebender/vello/pull/1861),
  [atlas-drop diagnostics](https://github.com/linebender/vello/pull/1862), and
  [toolchain-compatible lint attributes](https://github.com/linebender/vello/pull/1863).
  This keeps diagnostics and compatibility cleanup out of the performance PR
  while retaining the dependency-ordered 0.9 history used by Tilo.
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

The Tilo build does not depend on these refs. They are review/export artifacts;
the existing portable path overrides remain authoritative until the upstream
changes are accepted and the vendoring can be removed.

## Published forks

The SDK's own crates depend on three forks under their own crates.io names,
so the SDK can be published (`tools/sdk/publish_check.py`): `tilo-winit`
(`vendor/winit`), `tilo-accesskit-winit` (`vendor/accesskit_winit`, the
unchanged 0.34.0 release built on tilo-winit) and `tilo-vello`,
`tilo-vello-encoding`, `tilo-vello-shaders` (`vendor/vello*`). Each keeps its
upstream library name, so code still writes `winit::`/`vello::`, and each has
a `TILO-FORK.md` naming upstream and what differs. Their manifests differ
from the crates.io originals only in that publication identity (name,
description, readme, packaged files, renamed dependencies);
`tools/verify-vendor-trees.py` sets those lines aside.

No crates.io crate in the workspace depends on `winit` any more (the shell
uses `tilo-accesskit-winit`, and Smithay's `backend_winit` is off), so every
crate sees tilo-winit's types without a `[patch]`. vello had no crates.io
dependents either, so its `[patch]` entries are gone too.

The `wgpu-hal` patch stays a `[patch]`: a published SDK builds on the
crates.io release, and only the optional `presentation-feedback` feature
needs the patched Metal observer (now part of the `metal` feature, since a
published crate cannot name a feature the release lacks).

The `wgpu` patch is a `[patch]` too: `Device::poll` reports a lost device
instead of panicking (vendor/wgpu/TILO-PATCH.md). The compositor needs it to
survive a GPU reset; an SDK app built on the crates.io release still panics on
one, as it always has, and its process is restarted.

## Git forks

Some patches live in public forks on code.tilo.so rather than in this tree.
`[patch.crates-io]` names each fork and revision over https, and Cargo.lock
records them, so CI, image builds and apps outside this repository all fetch
the same revisions. To change one, push to the fork and move the revision in
`[patch.crates-io]` (and `.config/hakari.toml` for rustypipe).

| Fork | Branch | Revision | Crates | Change | Upstream contribution |
|---|---|---|---|---|---|
| [tilo/accesskit](https://code.tilo.so/tilo/accesskit) | `unix-optional-async-io-v0.23` | `a76088c4` | accesskit 0.25.0, accesskit_consumer 0.39.0, accesskit_atspi_common 0.20.0, accesskit_unix 0.23.0 | `zbus/async-io` only with accesskit_unix's `async-io` feature (the other three crates are the unchanged releases, patched so they aren't duplicated) | [accesskit-optional-async-io](https://code.tilo.so/tilo/upstream-contribs/src/branch/main/accesskit-optional-async-io) |
| [tilo/atspi](https://code.tilo.so/tilo/atspi) | `optional-async-io-v0.29` | `9183f58d` | atspi 0.29.0, atspi-common 0.13.0, atspi-proxies 0.13.0 | `async-io`/`tokio` features select zbus's backend instead of async-io always | [atspi-optional-async-io](https://code.tilo.so/tilo/upstream-contribs/src/branch/main/atspi-optional-async-io) |
| [tilo/rustypipe](https://code.tilo.so/tilo/rustypipe) | `reqwest-0.13-v0.11.4` | `aeb292fb` | rustypipe 0.11.4 | reqwest 0.13 and fancy-regex 0.16 | [rustypipe-reqwest-0.13](https://code.tilo.so/tilo/upstream-contribs/src/branch/main/rustypipe-reqwest-0.13) |

Remove a row and its `[patch.crates-io]` entries once upstream releases the
change.

## Vello changes

`vendor/vello` and `vendor/vello_encoding` are `vello` 0.9.0 and
`vello_encoding` 0.9.0 from crates.io, verbatim, except for the files
below and the publication identity in `Cargo.toml` (see "Published forks").
Everything else in both trees — including `Cargo.lock`, the licenses and
every other `.rs` file — is the unmodified crates.io release; diff either tree
against the published crate to confirm.

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

`BumpBudget::max_binding_bytes` caps every bump buffer, floors and growth
included, at the device's largest storage binding. `CEILING` puts lines and
segments at 8M x 24 bytes = 192 MiB and the lines floor grows with the path
tag count, both past WebGPU's default 128 MiB; a bind group past the limit
fails the device, where a clamped buffer drops geometry the shaders report
through `BumpAllocators::failed`.

`segments` is reserved separately from `seg_counts` here, where upstream sizes
them alike so its `seg_counts` check covered both. `coarse.wgsl` therefore
checks its `segments` allocation against `segments_size` and reports an
overflow as `STAGE_COARSE`; without it a frame whose segments outgrew their
reservation reported `failed=0` and was presented with geometry missing. `BudgetTracker::set_device_limits` sets it
from `device.limits()`, rounded down to a size class the engine's buffer pool
allocates exactly, since the pool rounds requests up and binds whole buffers.

Growth stops at `BumpBudget::ceiling_for_viewport`, not the flat `CEILING`:
the flat `blend_spill` ceiling (4M words) sat below one spilled blend level
per tile on a 5120x2880 desktop (14.7M), so every frame overflowed, was
refused growth, and never converged.

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

## `vendor/vello_shaders/shader/fine.wgsl`

The fine interpreter terminates on any PTCL stream. Upstream loops `while
true` until `CMD_END`, treats an unknown tag as a no-op that never advances,
and follows `CMD_JUMP` anywhere — including offset 0, which coarse writes on
PTCL overflow. A stream coarse did not fully write (a pooled buffer, an
overflow the fine sentinel missed) can therefore spin forever; on Safari's
WebGPU that hung the GPU and froze macOS's WindowServer until the watchdog
rebooted the machine. Fine now stops a tile after `MAX_TILE_COMMANDS`
commands, on an unknown tag, or on a jump that is not forward and inside the
buffer, and `fill_segment_count` clamps a fill's segment count to the
segment buffer. Valid streams render byte-identically (every GPU fixture in
`vendor/vello/src/lib.rs` passes unchanged).

## Total bump budget (`config.rs`, `bump_budget.rs`, `lib.rs`)

`BumpBudget::fit_total_bytes` shrinks every reservation by one factor until
the whole budget fits a byte limit; `BudgetTracker` applies it after each
growth, and `Renderer::set_bump_budget_limit` sets it. The per-binding clamp
alone still let seven buffers reach 128 MiB each; a browser's GPU process has
a hard memory limit (Safari's is 600 MB), and Safari's was killed at 842 MB
mid-frame, which hung the macOS window server.

## `vendor/vello/src/wgpu_engine.rs`: transient images are destroyed

A recording's freed images (the per-frame gradient ramp) are destroyed after
submission instead of only dropped. On WebGPU a dropped texture lives until
JavaScript collects it: a 60-window soak kept up to 1,100 of them alive.

## Image atlas limit (`image_cache.rs`, `resolve.rs`, `lib.rs`)

`Renderer::set_image_atlas_max` (through `Resolver::set_image_atlas_max` and
`ImageCache::set_max_size`) limits how far the image atlas grows; upstream's
maximum is 8192 px per side, a 256 MB texture. Images that do not fit after
eviction are omitted and reported, as they already were at the maximum.

## `vendor/vello/src/wgpu_engine.rs`: cleared pools and freed downloads are destroyed

wgpu's WebGPU backend does not destroy a buffer when it is dropped; the GPU
memory stays until JavaScript collects the object. `clear_pool` (run when the
bump budget grows) and `free_download` now destroy their buffers, so a budget
growth does not keep every old buffer resident beside its replacement.

## `vendor/vello_shaders/shader/*.wgsl`: no read before a workgroup array

Upstream computes an exclusive prefix as
`select(0u, counts[ix - 1u], ix > 0u)` in `backdrop_dyn.wgsl`, `coarse.wgsl`
(twice), `tile_alloc.wgsl` and `fine.wgsl` (twice). WGSL's `select` evaluates
both arms, so element 0 indexes `counts[-1]`: an out-of-bounds workgroup read
whose result depends on the driver's bounds policy. A driver that does not
clamp reads a neighbouring shared variable, which only `select` happens to
discard; one that faults on the address kills the dispatch. Each site now
reads the previous count inside an `if`, so index 0 never touches the array.
Results are unchanged wherever the read was harmless.

## `vendor/vello/src/lib.rs`: one init thread on GL

`Renderer::new` sets `num_init_threads` to one when the device is on the GL
backend. wgpu-hal's GLES device holds its single context lock for every
pipeline compile, so parallel initialisation gains nothing there, and on a
slow driver a queued worker thread exceeds the context lock timeout and
panics.

## `vendor/vello/src/lib.rs`, `wgpu_engine.rs`: images without CPU pixels

An image whose blob is empty is drawn, on a renderer without an
`override_image` texture for it, from the process's `ExternalImageSource`
(`set_external_image_source`, keyed by blob id) instead of panicking, and
transparent when the source has nothing. The compositor keeps Wayland
client frames on the GPU and draws them on its presenting renderer from
their textures; screenshots, recordings and renderers on other devices
reach the same frames through the source, which reads them back on demand.

## `vendor/vello/src/image_source.rs` (new), `lib.rs`, `wgpu_engine.rs`: images drawn from a texture

`Renderer::draw_image_from(image, Some(ImageSource { texture, origin,
opaque }))` makes an image's atlas write a small render pass instead of a
`copy_texture_to_texture`: the fragment shader `textureLoad`s the source at
`origin` and writes the atlas slot (viewport and scissor on the slot), forcing
alpha to one when `opaque`. The source may therefore be any colour format the
device samples as float — a Wayland client's `ARGB8888` buffer imports as
`Bgra8Unorm`, which `copy_texture_to_texture` cannot copy into the
`Rgba8Unorm` atlas — and an `X` format is drawn opaque, so the compositor
draws a client's dma-buf straight from its import with this one copy. The
atlas texture gains `RENDER_ATTACHMENT` usage for it. A source takes
precedence over an `override_image` texture for the same image.
`Renderer::image_source_writes()` counts the writes and their bytes.

## zlink-macros

`vendor/zlink-macros` is crates.io `zlink-macros` 0.7.0 (checksum
`2fc623b8323797bf2d48333e7633e143077dd92cc66d9cca170014b66c43b1b5`) with one
change in `src/service/codegen.rs`: the service macro's ReplyStreamParams enum
emits its variants sorted by type instead of in `HashMap` order, which differs
per process and made every build of a binary that expands the macro different.
It is not part of the Tilo workspace. The OS image's bootc build uses it
through `[patch.crates-io]` (os/external-assets/bootc-runtime/0002-*.patch), so
bootc builds reproducibly.

## `vendor/winit`: focused native keyboard configuration

The ordinary Wayland seat publishes an immutable `KeyboardLayoutChanged`
configuration at keymap, focus-enter and modifier transitions. It preserves the
received XKB masks/group and resolves effective symbols using that exact state,
with bounded keycode, symbol and string counts. This is keymap configuration,
not entered text or compose history; unfocused clients receive no new snapshots.
Settings uses it to label its foreground physical keyboard map independently
from logical key events. A real Smithay-to-Winit wire regression covers initial
keymap/focus and several queued modifier/group changes; no global capture or new
protocol/socket is introduced.
