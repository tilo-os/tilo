# Patches against upstream

## Rig Chat tool-name fragments

`rig-core` 0.44.0 is the checksum-verified MIT registry runtime crate. Its
OpenAI Chat decoder accumulates tool names per call, ignores empty names and
identical full-name restatements, and supplies the final name to both the replay
item and normalized writer. A Chat-only writer API streams arguments under an
unnamed presentation-only Start before closure; End supplies the final name.
The transcript validator accepts these unnamed tool Starts. Gateway previews
remain inert until the provider successfully finishes and authorizes the final
call. No request-tool prefix guessing, generic name concatenation,
other-provider change or second SSE decoder is introduced. `vendor/rig-core/PROVENANCE.md` records the
exact release/checksum and the upstream HEAD inspected for an existing fix.
Gateway wire fixtures and vendored Chat tests cover fragmented/interleaved names
and agreement across emitted, normalized and replay calls. Remove the patch
once upstream provides this behavior.

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

## DAVE for Discord voice

`davey` 0.1.5 is the checksum-verified MIT registry release, moved from
openmls 0.8 to the openmls 0.9 generation the workspace builds (the two pin
incompatible `hax-lib` versions). openmls 0.9 surfaces one's own commit as
`OwnPendingCommit` instead of failing with `OwnCommit`; davey merges its
pending commit on that variant. The Node and Python bindings are left out.
See `vendor/davey/PROVENANCE.md` and `TILO-PATCH.md`; the Discord worker's
voice tests run the full handshake against it.

## iroh idle timers

`iroh` 1.0.3, `netwatch` 0.19.1 and `iroh-gossip` 0.101.0 are the
checksum-verified registry releases in the lock, each with small patches
so an idle endpoint wakes for nothing: no re-STUN tick and no home relay
while the relay map is empty, no 15 s wall-clock poll on Linux (a timerfd the
kernel cancels on resume or a clock step replaces it), and no gossip cache
sweep once a topic's caches are empty, no address-lookup sweep while it holds
no addresses, and no membership shuffle without an active
neighbor. A returning neighbor rearms the shuffle once. With relays, a net
report that keeps finding the same addresses waits longer before the next, up
to five minutes, and paths may keep alive less often than every 5 s, which
tilo-fs uses for its held connections. Network changes still arrive through
netlink at once, and `netwatch` has the kernel drop the address-lifetime refreshes
a router sends every few seconds (a BPF filter on its netlink socket) instead of
waking the process to ignore them. Its monitor also re-reads the interfaces
once its netlink socket has subscribed, so a change made while it starts is
not lost. Each directory's `TILO-PATCH.md` has the details and an
upstream-ready description; `crates/system/fs/tests/iroh_idle.rs` pins them.
`iroh` and `iroh-gossip` build an `rlib` only: as path packages their
upstream `cdylib` dropped the hash from the library filenames, so two builds
in one Cargo run overwrote each other's outputs (E0460/E0463).
Remove a copy once its upstream release carries the change.

## Tables without relocations

`pulldown-cmark` 0.12.2 is the checksum-verified MIT registry release with one
change: its HTML5 entity table is offsets into two string literals rather
than 2125 pairs of pointers, so the shared app runtime (which carries it
through the Markdown codec) no longer relocates 68 KB of it in every app
process before `main`. Same entities, same order, same lookup; see its
`TILO-PATCH.md` and `PROVENANCE.md`. Remove the copy once upstream generates
the same layout.

`regex-syntax` 0.8.11 is the checksum-verified MIT/Apache-2.0 registry
release with its Unicode name tables (`BY_NAME`, `PROPERTY_NAMES`,
`PROPERTY_VALUES`) and simple case folding table rewritten as string
literals and integer offsets by `tools/regex-syntax-tables.py`: about 121 KB
the shared runtime no longer relocates in every app process. Every set,
name and fold is unchanged; see its `TILO-PATCH.md` and `PROVENANCE.md`.

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
- `codex/winit-dictation-ack-history` contains winit alone, based on crates.io
  `winit` 0.30.13. It retains the registry metadata and updates
  `Cargo.toml.orig` alongside Cargo's normalized manifest, making the topic
  commits straightforward to apply to an upstream source checkout.
  It extends `vendor-upstream/winit-0.30.13`: one exact synchronization records
  already-landed pen, presentation and keyboard source from Tilo's
  `2fa854fb55` base, followed by the content-free dictation ACK extension.
  The ACK is a separate reviewable commit, `217d0e69b3f4824391bdab8c5f73ce178cf4946c`.

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

The winit fork also carries `tilo_text_input_v1`, a content-free, surface-bound
acknowledgement beside standard Wayland text-input-v3. It publishes only opaque
field stamps and operation receipts. Dictation text still arrives as the standard
IME commit; this extension grants no microphone permission or input-method
ownership. The SDK enables deferred browser fields only during an admitted voice
session and consumes the original renderer's guarded receipt before acknowledging
an edit. `vendor/winit/TILO-FORK.md` records the additional event variant and API.

The `wgpu-hal` patch stays a `[patch]`: a published SDK builds on the
crates.io release, and only the optional `presentation-feedback` feature
needs the patched Metal observer (now part of the `metal` feature, since a
published crate cannot name a feature the release lacks).

`vendor/wgpu-hal/src/vulkan/adapter.rs` also asks for
`VK_EXT_image_drm_format_modifier` (Vulkan 1.2+, next to the external-memory
extensions it already enables) when the device has it. `ui::dmabuf_import`
reads `Device::enabled_device_extensions` and, with the extension, imports a
client dma-buf with the producer's exact plane offset and row pitch; on the
crates.io release, which lacks it, the import keeps its linear-only path.
It also enables `VK_EXT_queue_family_foreign`, and a texture made with
`TextureMemory::ForeignExternal` (a client dma-buf import) makes its first
barrier an acquire from `VK_QUEUE_FAMILY_FOREIGN_EXT`.
It enables `VK_KHR_external_semaphore_fd` when the device has it, and
`vulkan::Queue::add_wait_semaphore` makes the queue's next submission wait for
a binary semaphore: `ui::dmabuf_import::wait_for_writes` imports a client
dma-buf's pending write fence (a sync file) into one, so the GPU reads the
buffer only after the client's writes, without a thread waiting.

The `wgpu` patch is a `[patch]` too: `Device::poll` reports a lost device
instead of panicking (vendor/wgpu/TILO-PATCH.md). The compositor needs it to
survive a GPU reset; an SDK app built on the crates.io release still panics on
one, as it always has, and its process is restarted.

## Accessibility and YouTube manifests

Four crates differ from their crates.io release only in `Cargo.toml`. Each
directory's `PROVENANCE.md` has the exact edit. They used to be git forks on
code.tilo.so, which made every Cargo command in the workspace fail whenever
that host was down. Now they are path crates here:

| Directory | Release | Change | Upstream contribution |
|---|---|---|---|
| `vendor/accesskit_unix` | accesskit_unix 0.23.0 | `zbus/async-io` only with the crate's `async-io` feature, `zbus/tokio` with `tokio` | [accesskit-optional-async-io](https://code.tilo.so/tilo/upstream-contribs/src/branch/main/accesskit-optional-async-io) |
| `vendor/atspi`, `vendor/atspi-common`, `vendor/atspi-proxies` | atspi 0.29.0, atspi-common 0.13.0, atspi-proxies 0.13.0 | `async-io`/`tokio` features select zbus's backend instead of async-io always | [atspi-optional-async-io](https://code.tilo.so/tilo/upstream-contribs/src/branch/main/atspi-optional-async-io) |
| `vendor/rustypipe` | rustypipe 0.11.4 | reqwest 0.13 and fancy-regex 0.16 | [rustypipe-reqwest-0.13](https://code.tilo.so/tilo/upstream-contribs/src/branch/main/rustypipe-reqwest-0.13) |

accesskit, accesskit_consumer and accesskit_atspi_common are the unchanged
crates.io releases. `vendor/miracast` is Tilo's own Miracast crate at a pinned
revision of its repository, vendored for the same reason until it is
published. Remove a row and its `[patch.crates-io]` entry once upstream
releases the change.

## Vello changes

`vendor/vello` and `vendor/vello_encoding` are `vello` 0.9.0 and
`vello_encoding` 0.9.0 from crates.io, verbatim, except for the files
below, the publication identity in `Cargo.toml` (see "Published forks") and
the `skrifa` requirement in both manifests: 0.44 instead of 0.42, the version
the toolkit, cosmic-text and swash use, so glyph outlines are drawn by one
copy of the font parser instead of two (about 230 KB of the web desktop's
start module). vello 0.9's use of skrifa compiles unchanged against 0.44.
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

`Renderer::settle_bump_budget`, for one-shot renderers that read the frame
back, waits until the counters' readback has landed, not a fixed four polls:
on the device the process shares, another thread's poll can be the one
running the readback's map callback after this thread's wait returned, and
a frame that overflowed was then taken for one that fit.
`settling_waits_for_counters_whose_callback_lands_on_another_thread` lands
the callback from another thread (through `test_helpers::hold_ready`) and
fails the four-poll settle.

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

## `vendor/vello/src/util.rs`: a surface without a blitter

`RenderSurface::blitter` is `Option<TextureBlitter>` instead of
`TextureBlitter`. `TextureBlitter::new` compiles a render pipeline (about
2.6 ms on a Snapdragon X), and every Tilo window made one on its way to the
first frame although a surface configured with `COPY_DST` in the target's
format (`Rgba8Unorm`, the common case) takes the target by a texture copy and
never blits. `ui::gpu::RenderContext::create_surface` makes the blitter only
for a surface that needs it, and `ui::gpu::draw_target_to_surface` copies or
blits accordingly. vello's own `RenderContext::create_surface` always makes
one (`Some`), so its behaviour is unchanged.

## `vendor/vello_encoding/src/encoding.rs`: append keeps forced state

Upstream `Encoding::append` sets `self.flags = other.flags`. A glyph run
writes its own transform into the resolved stream and sets
`FORCE_NEXT_TRANSFORM` so the next draw re-encodes its transform. Appending
an encoding that encodes no transform (an empty sub-scene) cleared that flag,
so the next draw whose transform equalled `transforms.last()` skipped it and
was placed with the glyph run's transform instead — in Chat, pictures painted
after a line of text landed off their card and never showed. `append` now
keeps our force bits when the appended encoding has no transforms (or
styles) of its own.

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

## `vendor/vello_shaders/shader/fine.wgsl`, `src/cpu/fine.rs`: translation-stable area coverage

Upstream evaluates a line's area coverage of a pixel as
`(b + 0.5 * (d * d - c * c) - xmin) / (xmax - xmin)`. For a vertical or
near-vertical edge, `xmax - xmin` is the `1e-6` nudge, while the two
squares and their sum round at the magnitude of x: the numerator carries an
absolute error of a few `1e-8` against a value of `1e-6`, so the edge pixel
comes out up to 15 channel steps (6 % coverage) off, by an error that depends
on the exact f32 bits of the tile-relative x. The same shape drawn a whole
number of pixels elsewhere then rasterises differently, which a compositor
that moves already rasterised pixels (Tilo's strip pans) shows as edge noise
against a fresh render. `fill_path`, `fill_path_ppt8` and the CPU fine stage
evaluate the algebraically identical
`((b - xmin) - 0.5 * ((c - d) * (c + d))) / (xmax - xmin)` instead: both
differences are of nearby values and exact, so coverage is accurate to f32
precision and the same at every whole-pixel offset (in the strip pan test,
whole renders at two pans went from up to 31 pixels more than a step apart
to none). Edge pixels move to their correct coverage, so visual goldens of
antialiased text change by up to that much. The accurate form also keeps
the slivers of coverage the `1e-6` nudges add next to an edge on a whole
pixel (about `1e-6`), which the expanded form happened to round to zero;
the store divided such a pixel's colour out to full strength under an alpha
that stores as 0. Fine therefore stores a pixel whose alpha rounds to 0 as
transparent black, which nothing composited can tell apart.

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

## Image cache eviction, shrink and stats (`image_cache.rs`, `resolve.rs`, `lib.rs`)

Upstream evicts a resident image only when an allocation fails, and never
shrinks the atlas. A Wayland client's frame is a new image on every commit,
so every superseded frame kept its CPU pixels alive until the atlas was full,
the atlas grew to its maximum on the way, and the compositor kept that
high-water mark for the rest of the session (#3971: about 14 MiB per app
opened and closed, past 1 GiB). Now:

- `ImageCache::finish_resolve` evicts every resident no pass drew in the last
  two (`EVICT_AFTER_GENERATIONS`, upstream's own staleness rule), so a resident
  survives two scenes that skip it — two outputs, or a capture beside the
  live frame — and a window that keeps committing holds three frames at most.
- When a pass's residents cover at most an eighth of the atlas for 60 passes
  in a row (`SHRINK_AFTER_RESOLVES`), the next pass first repacks into half
  the side, down to the initial 1024. Repacking marks every resident dirty, so
  each is uploaded again, at its new position, the next time it is drawn; the
  halving happens in `begin_resolve`, before the pass hands out positions. A
  repack that does not fit leaves the atlas as it was.
- `Renderer::image_cache_stats()` (through `Resolver::image_cache_stats`)
  reports `ImageCacheStats`: atlas side, resident count, resident CPU bytes
  and area, and lifetime evictions, shrinks and omissions.
- `Renderer::shrink_image_atlas_soon()` (through
  `Resolver::shrink_image_atlas_soon`, `ImageCache::shrink_soon`): the caller
  knows images went for good, a window closed. For the next 16 passes
  (`SHRINK_SOON_RESOLVES`) a pass after a sparse one halves the atlas at once,
  as often as the eighth-full rule allows, instead of after 60. A desktop that
  goes idle after a close draws a handful of frames, so the slow rule kept a
  256 MiB atlas after four apps closed.

Own tests in `image_cache.rs`: stale residents are released without
allocation pressure, an image survives two passes that skip it, a committing
window holds three frames at most, the atlas shrinks after residency drops and
uploads its residents again where it draws them, a busy atlas does not shrink,
thirty open/close cycles leave only the wallpaper resident at the size it
started with, and told images went the atlas comes down within a few passes
while an expired request waits the long way again.

- `ImageCache::evict_superseded`: once an image does not fit and the atlas is
  at its maximum, residents the current pass has not drawn go at once instead
  of after two passes. A client frame over half the atlas's side (a 4K buffer
  rendered at twice the output's density) cannot sit beside the frame it
  replaces, and the new frame was omitted: the tile drew its backdrop only.

## Scratch shrink (`bump_budget.rs`, `wgpu_engine.rs`, `lib.rs`)

Upstream and the bump-budget patch above size a renderer's working memory for
the busiest scene it has drawn and keep it: the bump reservation only grows,
and the buffer pool keeps every size class until something clears it. A
compositor that once drew several windows therefore kept their scratch after
they closed (about 35 MiB on the dev VM: a 36 MiB reservation against the
bare desktop's 28, and a 68 MiB pool). `Renderer::set_shrink_scratch(true)`
opts a renderer in; it is off by default, so applications keep upstream's
behaviour.

- `BudgetTracker` tags each counter readback with whether its render ran
  coarse over the whole scene (`begin_dense_readback`, used by
  `render_to_texture_with_post_render_checked{,_indexed}`); a damage render's
  counters cover only the bins it redrew, so they neither count nor shrink.
  It keeps the field-wise maximum of the dense, non-overflowing frames since
  the reservation last changed. After 120 of them (`SHRINK_AFTER_FRAMES`) it
  lowers each field towards four times that maximum (`SHRINK_HEADROOM`, twice
  what growth reserves), by at most half per shrink, so the scene that grew
  a field still fits once and draws without overflowing when it comes back.
  A shrink that frees less than an eighth of what the viewport reserves,
  floors included (`SHRINK_MIN_FRACTION`), is not taken. Growth or an
  overflow starts the window again. The caller clears the pool on a shrink
  as it does on growth (`collect` reports both as `changed`).
- `ResourcePool` stamps each free buffer with the recording that returned
  it; after each recording an opted-in engine destroys the free buffers no
  recording has taken for 120 recordings (`POOL_KEEP_RECORDINGS`), with the
  cached bind groups that name them. Buffers are taken from the end of their
  class's list, so a class's surplus goes first.
- `Renderer::shrink_scratch_soon()`: the caller knows the scene got smaller
  for good, a window closed. The next shrink needs only two dense frames
  drawn since (`SHRINK_SOON_FRAMES`, within 16 measurements), once, and for
  the next 64 recordings free buffers go after 16 untaken. An idle desktop
  draws a handful of frames after a close, too few for the slow rule.
- `Renderer::scratch_shrinks()` counts the shrinks, for the caller to have
  freed device memory returned (a software rasteriser keeps it otherwise).

Own tests in `bump_budget.rs` (a window of lighter dense frames shrinks it by
at most half and the busy scene still fits, damage renders neither count nor
shrink, an overflow or a growth starts the window again, a soon request
shrinks once after two dense frames drawn since, small steps and renderers
that did not opt in hold) and in `lib.rs` on a GPU (the shrunk renderer draws
the light scene bit-identically to a fresh one, holds less, and draws the
busy scene again in one render, identically; buffers no recording takes are
destroyed and the pool holds what the light scene alone pools).

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

## `vendor/automerge`: parse changes without applying them

`Automerge::parse_changes(data)` parses and checksums the change chunks of an
incremental save without applying them to a document, so a receiver reads
each change's hash, message and dependencies (the signed claim of what each
change is) before it decides to merge them.

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

## `vendor/vello_shaders/build.rs`, `vendor/vello/src/lib.rs`: shader fingerprint

The build script also emits `SOURCE_FINGERPRINT`, an FNV-1a hash over every
shader's name and WGSL text in name order, and vello re-exports it as
`vello::SHADER_FINGERPRINT`. A `wgpu::PipelineCache` an app keeps across
launches (`ui::pipeline_cache`) is keyed by it, so a cache filled by other
shaders is never used. Nothing else reads it.

## `vendor/vello_shaders/build.rs`: minified embedded WGSL

The preprocessed shaders the build script embeds (`WgslSource::code`) and
fingerprints are minified: comments, indentation and blank lines removed
(`minify_wgsl`). Upstream embeds them as written, which is mostly
explanation; every binary carried it, the web page's start module included
(1.07 MB of WGSL, 0.62 MB minified). WGSL has no string literals, so `//` and
`/*` always open a comment. The build script compiles every minified shader
again and fails unless it validates with the same workgroup size, bindings and
workgroup buffers as the original (`check_minified`). The MSL translation
still reads the original source.

## `vendor/vello/src/lib.rs`, `shaders.rs`, `wgpu_engine.rs`: deferred pipelines

`Renderer::new_deferring(device, options, deferral)` registers every shader
as `Renderer::new` does, but builds a pipeline before returning only when
`deferral` allows. `PipelineDeferral::Unused` defers the registrations a full
frame on the default path never dispatches: the three-level path-tag scan,
the GPU draw reduce and leaf while the CPU draw prefix serves, the indirect
path stages while the direct ones serve, the clip reduce and the sparse fine
stage. `PipelineDeferral::All` defers every one, for a caller that rasterizes
its frames another way. `start_deferred_pipelines` builds them in the
background on the renderer's init threads, the default-path ones first and the
largest of each group first, and runs a callback after the last. Before
encoding, a render installs what the thread has finished. For a deferred
pipeline it dispatches and that is not built yet, it waits for the thread, or
builds it itself when no thread is running, so a dispatch never lacks its
pipeline. On GL nothing is deferred (one context lock, see "one init thread
on GL"). `Renderer::new` is unchanged except that the parallel initial build
starts the longest shaders first. `deferred_pipelines()` reports what is
pending and what a render had to build itself.

`Renderer::new_async_deferring(device, options, deferral)` is the same for
`new_async`, which the browser uses: it awaits only the pipelines `deferral`
does not defer. `start_deferred_pipelines_async` then starts every deferred
pipeline through `create_compute_pipeline_async` without waiting. Renders poll
those compiles before encoding and install the finished ones. A render can't
wait for a promise, so a deferred pipeline it dispatches whose compile is not
in yet is created synchronously, and the GPU process compiles it before the
dispatch. This works on every target; native windows keep the threads.
`new_async_deferring_reporting` takes a callback that runs each time one of
the awaited pipelines is compiled, so the browser can bound a stalled compile
without failing one that is slow but still finishing pipelines.

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

## `vendor/winit`: a field's whole text-input content type

`WindowExtWayland::set_ime_content_type` sends any `zwp_text_input_v3`
content hint set and purpose (an email field's `email` purpose, a name's
title case, a password's hidden and sensitive hints) instead of the three
that `ImePurpose` stands for. It replaces the earlier multiline-only setter:
multiline is one of the hints. Other platforms keep `set_ime_purpose`, and
`None` returns Wayland to the purpose's own mapping.

## `vendor/winit`: buffers another process drew

A Tilo app's frames are drawn by its room's render service
(`docs/architecture/app-rendering.md`), and the app attaches the service's
buffers to its own window. `types/client_buffers.rs` binds
`zwp_linux_dmabuf_v1` at version 3 and keeps the (format, modifier) pairs
its `modifier` events announce; `WindowExtWayland::create_client_buffer`
makes a `wl_buffer` of a dma-buf (only a pair the compositor announced, so
asking is never a protocol error) or of `wl_shm` memory, calling its owner
back on every `release`; `attach_client_buffer` attaches, damages and
commits under the window state's lock, so no configure or input-method
commit falls between attach and commit, with optional `wp_presentation`
feedback, and flushes from the calling thread. `client_buffer_formats`
reports the announced pairs. X11 windows report none and make no buffers.

## `vendor/winit`: drag offers stream files when asked

`WindowExtWayland::start_drag` takes each offer as a `DragData`: bytes, or a
file. A file is opened and copied into the pipe when a receiver asks for its
mime type, on the thread that already writes the pipe, so starting a drag
(inside a pointer motion on the app's thread) reads nothing: Files froze for
seconds holding a multi-gigabyte video in memory before the gesture began.
Offers are held behind an `Arc`, so a request no longer copies a payload.

## `vendor/winit`: activation tokens carry the press they follow

On Wayland, `Window::request_activation_token` sets the serial and seat of the
last press in the window (a pointer button press, a key press or a touch
down) before committing the request. Upstream sets only the surface, so a
compositor that checks the serial, as `xdg_activation_v1` lets it, refuses
every token a native app asks for. The press is recorded per window from the
input handlers; releases, motion and focus changes are not presses and are
not recorded. A token still proves only what the compositor checks.

## Build-script-generated protobuf descriptors

`buffa-build` 0.9.2 is carried as verified registry source with a small
upstream-compatible change: a descriptor proven to be produced inside the
caller's Cargo OUT_DIR is not registered as an input. All external descriptors
remain tracked, including symlinks across the output-directory boundary.
`vendor/buffa-build/PROVENANCE.md` records the checksum, revision and patch.
The registry waproto source and its source/hash guards remain unchanged.
