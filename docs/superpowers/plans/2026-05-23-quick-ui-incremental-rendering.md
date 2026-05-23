# quick-ui incremental rendering plan

> **Status:** ready-to-dispatch. Each unit below is a self-contained
> subagent prompt. Units are mostly ordered (the cache layer depends
> on the redraw-gate API), but Unit 4 can run in parallel with the
> later units.

## Goal

Take the toolkit from "re-render the entire widget tree on every
event" to "only re-render what actually changed." Hard target: at
rest, an idle window pumps **zero** GPU work per second; a single
widget mutation re-encodes only that widget's local scene fragment
and submits it as part of a normal Vello composition. The user's
device should be able to sustain `≥ 1000 Hz` of widget mutations
end-to-end (we won't actually run the display at 1000 Hz — vsync
caps it — but the headroom should be there).

Today's baseline (measured by the `widget_perf_bench` example in
the `cdc5793` wip commit; the showcase tree, 50 widgets, click on
the Day/Week/Month `ToggleGroup`):

```
                      release   debug
handle_event             17 µs    55 µs
compute_layout           10 µs    22 µs
apply_layout              9 µs    51 µs
draw_to_scene           725 µs  7552 µs
vello render           7278 µs  9439 µs
TOTAL                 ~8.0 ms ~17.1 ms
```

> Layout is **not** the bottleneck (was the hypothesis from the
> previous session; the bench falsified it). `draw_to_scene` (the
> widget tree walking + Vello primitive emission) and `vello
> render` (GPU encode + submit + present) dominate.

A 1000 Hz frame budget is 1 ms — so we need to remove almost all of
both phases from the steady-state path when state hasn't changed,
and shrink them dramatically when only one widget changed.

## Architecture overview

Five layers, each landed in its own unit:

```
              ┌─────────────────────┐
              │  Widget mutation    │  e.g. ToggleGroup click
              └────────┬────────────┘
                       ▼
              ┌─────────────────────┐
   Unit 1  →  │  needs_redraw / is_layout_dirty signals │
              │  (per-widget, opt-in)                   │
              └────────┬────────────┘
                       ▼
              ┌─────────────────────┐
   Unit 2  →  │  App redraw gate                        │
              │  (skip request_redraw, compute_layout,  │
              │   and GPU submit when no widget signals) │
              └────────┬────────────┘
                       ▼
              ┌─────────────────────┐
   Unit 3  →  │  Per-widget scene cache                  │
              │  (each widget owns a Scene; App composes │
              │   them with vello::Scene::append)         │
              └────────┬────────────┘
                       ▼
              ┌─────────────────────┐
   Unit 4  →  │  text-measure caching                    │
              │  (cosmic-text shaping is the per-widget  │
              │   hot path; cache by (text, size))       │
              └────────┬────────────┘
                       ▼
              ┌─────────────────────┐
   Unit 5  →  │  Dirty-rect GPU submit (stretch)         │
              │  (only render the bounding box of        │
              │   actually-changed widgets)              │
              └─────────────────────┘
```

Each unit is independently shippable — Units 1+2 alone should drop
idle CPU+GPU to near zero. Units 3+4 attack the click-to-pixels
latency.

---

## Unit 1: per-widget needs_redraw + layout_dirty signals

**File scope:**
- `quick-ui/src/widget/mod.rs` — extend the `Widget` trait with
  two new opt-in methods.
- `quick-ui/src/widget/{column,row,container,scrollable}.rs` —
  recursive default impls that bubble children's signals up.

**What changes in the trait**

```rust
pub trait Widget {
    // ... existing methods ...

    /// Whether this widget's *visual* state has changed since the
    /// last successful `draw` call. Default true (current behaviour).
    /// Widgets that want incremental redraw opt in by:
    ///   1. Returning `false` here by default.
    ///   2. Setting a private `dirty` flag inside `handle_event` /
    ///      `sync` whenever they mutate any field that affects
    ///      `draw`.
    ///   3. Clearing the flag inside `mark_drawn()` (called by the
    ///      App after a successful `draw_with_presence`).
    fn needs_redraw(&self) -> bool { true }

    /// Called by the App after the widget's scene has been written
    /// into the composed frame scene. Widgets clear their `dirty`
    /// flag here.
    fn mark_drawn(&mut self) {}

    /// Whether this widget's *layout* has changed since the last
    /// successful `apply_layout`. Default true (back-compat). A
    /// widget whose intrinsic size never changes (Button label
    /// fixed, ToggleGroup selected swap) should return `false`.
    fn is_layout_dirty(&self) -> bool { true }
}
```

> NB: the existing `take_structure_dirty` covers child *insertions /
> removals*; `is_layout_dirty` is for *size* changes that don't add
> or remove nodes (e.g. TextInput grows wider as the user types).
> Both signals feed into the App redraw gate, but they're tracked
> separately because the work they gate is different (taffy node
> add-remove vs. compute_layout re-flow).

**Container forward:** `Column::needs_redraw` →
`self.children.iter().any(|c| c.needs_redraw())` (same for `Row`,
`Container`, `Scrollable`). Likewise for `is_layout_dirty`.

**Tests:** a leaf widget that opts in (`needs_redraw` returns true
on construction, false after `mark_drawn`, true again after a
state-mutating method) — pinned with a small custom widget inside
`#[cfg(test)]`.

**Risk:** widgets that *don't* override the new methods will keep
the `true` default, so they redraw every frame as today. The
incremental win only applies to widgets that opt in. Migrate the
expensive widgets first (Unit 3's list).

---

## Unit 2: App redraw gate

**File scope:** `quick-ui/src/app.rs`.

**Change three event-driven paths so they gate on the new
signals:**

1. **`window_event` handled-event branch** (currently line ~999
   `app.layout_dirty = true; window.request_redraw()`):

   ```rust
   if handled {
       // ... existing commit_action / needs_sync logic ...
       if app.root_widget.is_layout_dirty() {
           app.layout_dirty = true;
       }
       if app.root_widget.needs_redraw() {
           window.request_redraw();
       }
   }
   ```

   For events that don't change anything visible (hovering over an
   empty area, releasing a mouse button on an already-unhovered
   widget) the redraw is *skipped entirely*. Idle CPU + GPU drops
   to zero.

2. **`Resized`** (line ~561): keeps the unconditional
   `layout_dirty = true` + redraw — the surface size genuinely
   changed.

3. **`about_to_wait`** (line ~1080): the `needs_commit` /
   `take_structure_dirty` blocks also gate on `needs_redraw`
   before calling `request_redraw`.

4. **Inside `RedrawRequested`**: after `apply_layout` and
   `draw_with_presence`, call `app.root_widget.mark_drawn()` so
   the next frame's `needs_redraw()` returns false again.

**Behaviour change** (test in `app.rs` if practical, otherwise an
integration test):
- Constructing an App with all-static widgets and pumping 1000
  no-op `CursorMoved` events should produce **zero** redraws.
- A single ToggleGroup click should produce exactly one redraw.

**Risk:** widgets that forget to set their `dirty` flag will
become "stuck" in an unredrawn state. Default-true `needs_redraw`
protects us — only opt-in widgets need to be correct.

---

## Unit 3: per-widget scene cache

**File scope:** `quick-ui/src/widget/mod.rs` (trait), then one
follow-up commit per migrated widget.

**Trait extension**

```rust
pub trait Widget {
    // ...

    /// Optional: return a cached `Scene` representing this widget's
    /// last `draw` output. The App appends this fragment into the
    /// composed frame scene via `Scene::append` instead of calling
    /// `draw()`. Default: `None`, falling back to the live `draw()`
    /// every frame.
    fn cached_scene(&mut self) -> Option<&Scene> { None }
}
```

**Per-widget pattern** (showcase: `ToggleGroup`):

```rust
pub struct ToggleGroup {
    // ... existing fields ...
    cached: Scene,
    dirty: bool,
}

impl Widget for ToggleGroup {
    fn needs_redraw(&self) -> bool { self.dirty }
    fn mark_drawn(&mut self) { self.dirty = false; }

    fn handle_event(...) {
        // any mutation of `selected` / hover state sets:
        self.dirty = true;
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, ...) {
        if self.dirty {
            self.cached.reset();
            // ... emit primitives into self.cached, transformed
            // to widget-local space (Affine::IDENTITY) ...
        }
        scene.append(&self.cached, Some(transform));
    }
}
```

> **Critical:** the per-widget scene is emitted in *widget-local*
> coordinates so it's transform-invariant. The App passes the
> widget's screen-space `Affine` into `Scene::append`. Re-using a
> cached scene across `apply_layout` calls is then safe even when
> the widget's screen position changes (e.g. window resize).

**Migration order** (high → low payoff, measured by % of
`draw_to_scene` time in the showcase bench):

1. `data_table` — 50+ rows, heaviest emission.
2. `tree_view` — variable rows, depth-indented.
3. `color_picker` — SV square + hue strip + sliders → ~12 fills.
4. `date_picker` — 42-cell grid.
5. `tabs::TabBar` — per-pill drop shadow (4 rings each).
6. `toolbar` — per-item button.
7. `breadcrumb` — small win; do for symmetry.
8. Everything else can stay on the default un-cached path; the win
   tapers fast after the heavy widgets.

> Spinner / ProgressBar (indeterminate) deliberately stay un-cached
> because they animate every frame. They have to opt **out** of the
> cache. The default `needs_redraw = true` covers this; they just
> never implement `cached_scene`.

**Tests:** for each migrated widget, two unit tests:
1. *Cache invalidates on state change.* Construct, draw once,
   capture `cached_scene` clone, mutate, draw again, assert the
   new `cached_scene` differs.
2. *Cache reuses across draws.* Draw twice without mutation,
   assert the scene was only re-emitted once (instrument the
   widget with a `draw_count: u32` counter behind `#[cfg(test)]`).

**Risk:** widget state changes that *don't* visually change the
scene (e.g. recomputing `char_x_positions` in TextInput as part of
draw) must not be tracked as `dirty` — otherwise we cache nothing
and pay encode cost anyway. Each widget's migration commit must
audit which fields actually feed `draw`.

---

## Unit 4: text-measure caching

**File scope:** `quick-ui/src/text_render.rs`.

cosmic-text shaping (`Buffer::shape_until_scroll`) is the per-
widget hot path inside `draw_to_scene`. Many widgets call
`measure(text, size)` once per draw and `draw_text(...)` right
after — that's two full shapes for the same string.

**Change:** `TextRenderer::measure` and `draw_text` look up a
cache keyed by `(text.to_string(), size as u32, weight)`. The
cache stores the shaped `Buffer` (or just the `(width, height)`
pair for `measure`; the shaped glyphs for `draw_text`).

```rust
pub struct TextRenderer {
    // ... existing fields ...
    measure_cache: HashMap<MeasureKey, (f32, f32)>,
    /// Generation counter incremented every time the user's locale
    /// or the active font set changes (i18n switch, font swap).
    /// Cache keys include the generation so a swap invalidates
    /// in one shot.
    cache_generation: u64,
}
```

**Cache invalidation:** `i18n` changes (`set_locale`) and font-
hotload paths bump `cache_generation`. The cache is `HashMap`-
trimmed to a `LRU_LIMIT` (start with 4096 entries; ~3 KB / entry
of Buffer state).

**Don't cache shaped glyphs across `cache_generation`** — would
keep stale font handles alive.

**Tests:** pin the cache hit on a measure / measure / measure
sequence for the same `(text, size)` (the `measure_cache_size`
accessor returns 1, three measures later still 1). Pin
invalidation on `bump_generation`.

**Risk:** the cache could hide a regression where a widget's
visual state depends on `measure_weighted` returning *fresh*
metrics (e.g. between font swaps). Pin font-swap invalidation
explicitly.

---

## Unit 5: dirty-rect GPU submit (stretch)

**File scope:** `quick-ui/src/app.rs` + a tiny helper inside
`quick-ui/src/renderer.rs` (or wherever the surface-render call
lives now — `app.rs`'s `RedrawRequested` block).

Even after Units 1-4 land, the GPU submit (`render_to_surface` +
`surface_texture.present()`) re-uploads + composes the full
window every frame. For a single ToggleGroup click that's wasted
bandwidth.

**Approach:** track a "dirty bounding box" as widgets mark
themselves dirty:

```rust
fn dirty_bounds(w: &dyn Widget) -> Option<Rect> {
    if !w.needs_redraw() { return None; }
    let mut bounds = w.get_rect();
    for c in w.children() {
        if let Some(child_bounds) = dirty_bounds(c) {
            bounds = bounds.union(child_bounds);
        }
    }
    Some(bounds)
}
```

When all widgets are clean, skip `render_to_surface` entirely — the
swapchain still holds the last frame.

When some widgets are dirty:
- Vello 0.2.1 doesn't have a clipped/partial render path; the
  whole scene encoding is rasterised.
- We *can* clip the composed scene to the dirty bounds via
  `Scene::push_layer(BlendMode::default(), 1.0, transform, dirty_rect)`
  before appending the per-widget scenes, then pop. This prevents
  the *content* outside dirty bounds from being re-rasterised
  (Vello's tile binning skips empty tiles).

This is the *least* well-understood unit — Vello's actual cost
profile under partial-clip is mostly empirical. Land Units 1–4
first, measure, then decide whether Unit 5 is worth the engineering.

**Tests:** N/A this round — would need a frame-time bench against
the showcase to even know if the win is real.

---

## Cross-cutting: bench + regression gates

Keep `quick-ui/examples/widget_perf_bench.rs` as the primary
benchmark. Add three more iterations of the same headless harness
*before* shipping the units:

- `widget_perf_idle_bench`     — same tree, no events. Expected:
  zero redraws after the warm-up frame, total CPU ≈ 0.
- `widget_perf_typing_bench`   — drive 100 character insertions
  into the showcase's TextInput. Expected: each char triggers
  exactly one redraw of just the TextInput.
- `widget_perf_resize_bench`   — three Resized events. Expected:
  full re-flow each time (no caching wins here).

Each bench prints the same `mean / min / max` per-phase table as
`widget_perf_bench` so we can diff release-vs-release.

**Regression gate:** in CI, fail the build if any of the four
benches' total mean grows by `> 20 %` vs the most recent commit
on `main`. (Optional; lands after Unit 2 so there's a baseline to
compare against.)

---

## Dispatch checklist

For each unit dispatched as a subagent:

1. Worker's prompt includes the full unit body + the standard
   preamble (worktree setup, distrobox build env).
2. Worker commits its diff before exiting (in-tree; single
   commit per unit).
3. Worker report includes:
   - Lines changed, files touched.
   - Test names added + PASS/FAIL count.
   - Before/after numbers from `widget_perf_bench` for Units 3+4.
4. Coordinator runs `cargo test -p quick-ui --lib` after each
   unit's cherry-pick to confirm no widget broke.

---

## What's deferred

- **Vello incremental rendering** — Vello 0.5+ has an
  `IncrementalRenderer` that diffs scene encodings frame-to-frame.
  We're pinned at 0.2.1 (released alongside wgpu 0.20); upgrading
  to 0.5+ pulls in a wgpu 24+ requirement which the rest of the
  workspace (quick-shell screencast pipeline) isn't ready for.
  Re-visit once wgpu 24 lands across the workspace.
- **Animation off-thread** — Spinners + indeterminate progress
  bars currently force a redraw every frame. With Unit 1+2 they'd
  *still* force a redraw every frame because their `needs_redraw`
  returns true. A future plan can offload these to a tiny
  per-widget Animation thread that mutates the cached scene
  off-line and signals the App.
- **GPU-side glyph cache** — Vello already caches glyphs per font.
  cosmic-text's swash cache is the bottleneck above it; Unit 4
  addresses the bulk of that, but a true GPU-side atlas would
  shave more.
