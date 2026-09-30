# Profiling & performance testing `ui` apps

Every Tilo app is built on the `ui` toolkit and runs the same five-pass frame —
`sync`, `build_layout`, `apply_layout`, `draw`, `handle_event` (see
`crates/sdk/ui-core/src/widget/mod.rs`). The profiler measures each of those passes so an app's
frame cost can be **read off, not guessed**, and the headless harness guards
against a regression by driving a scripted interaction and reporting frame-time
statistics.

It lives in one place — `crates/sdk/ui-core/src/profiling.rs` — and is wired into `ui::App`, so
every app can use it. There is no per-app code: an environment
variable turns it on, and when it's off nothing on the hot path reads a clock or
allocates.

There are three surfaces, all sharing one atom, `FrameTiming` (per-pass
microseconds + frame index + a monotonic timestamp).

---

## 1. In-app HUD

Turn on the on-screen profiler for any app:

```bash
TILO_PROFILE=1 <app>            # e.g. TILO_PROFILE=1 cargo run -p ui-examples --example ui-lab -- gallery
TILO_PROFILE=1 terminal
```

A panel appears top-right with:

- the current frame's total time, an fps estimate, and — over budget — a red header;
- windowed **mean / max** frame time and an **over-budget count** across the last ~4 s;
- the per-pass breakdown (`sync`, `build`, `apply`, `draw`, `event`) in ms;
- a rolling **bar graph** of recent frame totals, with the **16.6 ms / 60 fps
  budget line** drawn across it and **over-budget bars painted red**.

The HUD is painted *after* the frame's timing is committed, so it never inflates
the numbers it reports. Unlike the debug overlay (`TILO_UI_DEBUG`), `TILO_PROFILE`
is **not** gated on `debug_assertions` — you profile release builds, because
that is where frame cost matters.

---

## 2. Machine-readable dump

Append one record per frame to a file for offline analysis:

```bash
TILO_PROFILE_OUT=/var/home/you/frames.jsonl <app>   # JSON-lines (default)
TILO_PROFILE_OUT=/var/home/you/frames.csv    <app>   # CSV (by extension)
```

`TILO_PROFILE_OUT` enables profiling on its own; combine it with `TILO_PROFILE=1`
to also show the HUD. Each line/row carries:

```
frame, t_us, sync_us, build_layout_us, apply_layout_us, draw_us, handle_event_us, total_us
```

`t_us` is a monotonic counter (microseconds since the profiler started), not a
wall clock, so it is immune to clock steps.

Every committed frame also emits a `tracing` event under the **`tilo::profile`**
target with the same fields, so an external tracer can consume the numbers with
no file:

```bash
RUST_LOG=tilo::profile=debug TILO_PROFILE=1 <app>
```

(A single structured event per frame is used rather than a span per pass — one
event carries the whole breakdown and keeps the overhead to nothing.)

### Reading the JSON-lines dump

```bash
# p95 total frame time, in ms
jq -s 'map(.total_us) | sort | .[(length*0.95)|floor] / 1000' frames.jsonl
# frames over the 16.6 ms budget
jq -s 'map(select(.total_us > 16667)) | length' frames.jsonl
```

---

## 3. Headless perf-scenario harness (the "performance testing")

This is what lets you measure a fix objectively and fail a test if a later
change slows a pass down. It drives the app's widget passes with no GPU
or display, using the same `ui::headless::Driver` the persistence harness uses.

### Write a scenario

A `PerfScenario` is a name, a viewport, a warmup count, and a list of steps:

```rust
use ui::profiling::{PerfScenario, PerfStep, run_scenario_on};

// Convenience constructor: warm up, then scroll 120 notches.
let scenario = PerfScenario::scroll("list-scroll", 120);

// Or build one explicitly:
let scenario = PerfScenario {
    name: "compose".into(),
    viewport: (1280, 800),
    warmup: 3,
    steps: vec![
        PerfStep::ClickNamed("compose_button".into()),
        PerfStep::Type("hello world".into()),
        PerfStep::Scroll { dy: -3.0, count: 60 },
        PerfStep::Frames(30),
    ],
};
```

Steps: `Frame`, `Frames(n)`, `Scroll { dy, count }`, `Type(String)`,
`Key(String)`, `ClickNamed(String)`, `ClickAt(x, y)`. Warmup frames are run and
discarded before measurement, so first-frame allocation / shaping-cache warmup
doesn't skew the numbers.

### Run it against any app

One call, over the app's root widget + `State`:

```rust
let report = ui::profiling::run_scenario_on(state, app_root(), &scenario);
println!("{report}");                 // human table
std::fs::write("report.json", report.to_json())?;   // machine JSON
```

If you already hold a `Driver`, call `ui::profiling::run_scenario(&mut driver,
&scenario)` instead.

### Run it from the CLI

The `perf_scenario` example is the ready-made runner (scrollable list + the
reference component gallery):

```bash
cargo run -p ui-examples --release --example ui-perf -- perf_scenario
cargo run -p ui-examples --release --example ui-perf -- perf_scenario --count 240 --json
```

### Guard against regressions in a test

```rust
#[test]
fn scroll_stays_under_budget() {
    let report = ui::profiling::run_scenario_on(state, app_root(),
        &ui::profiling::PerfScenario::scroll("scroll", 60));
    assert!(report.total.p95_us < ui::profiling::FRAME_BUDGET_US,
        "p95 frame time regressed: {}us", report.total.p95_us);
}
```

---

## Reading the stats

`run_scenario` returns a `PerfReport`:

```
perf scenario "list-scroll": 120 frames, budget 16.7ms, over-budget 0 (0.0%)
  pass (ms)         mean      p50      p95      min      max
  sync             0.000    0.000    0.000    0.000    0.001
  build_layout     0.812    0.795    0.980    0.700    1.240
  apply_layout     0.140    0.135    0.180    0.120    0.260
  draw             1.905    1.880    2.210    1.700    2.900
  handle_event     0.005    0.004    0.007    0.003    0.020
  TOTAL            2.870    2.830    3.310    2.640    4.100
```

- **mean / p50 / p95 / min / max** — per pass, in ms. Watch **p95**: it is the
  tail a user actually feels, and the number to assert against.
- **over-budget** — frames whose total exceeded 16.6 ms, i.e. would drop below
  60 fps. This is the "dropped frames" count.
- Per-`FrameTiming` samples are on `report.samples` if you want the raw series.

### Live vs. headless totals

- **Live app** (`total_us`): the measured wall time of the whole frame build, so
  it includes the small amount of work *between* passes.
- **Headless harness**: passes run contiguously, so the total is their sum.

One more fidelity note for the headless harness: the `Driver` rebuilds its
layout tree every frame, so `build_layout` is measured at its cold cost, whereas
the live app skips the rebuild on a clean frame. That makes headless
`build_layout` a conservative upper bound — which is exactly what you want a
regression guard to hold, and the per-pass split still isolates `draw` from
layout from input cleanly.

---

## Zero cost when off

When neither `TILO_PROFILE` nor `TILO_PROFILE_OUT` is set, `App` holds no profiler
and every render-loop hook is an `Option::map` over `None`: no clock is read and
nothing is allocated on the hot path. The same is true of the headless `Driver`
until `begin_profiling` installs a capture profiler.

## System-level counterpart

This measures a single app's `ui` frame passes. To drive and observe the whole
compositor headlessly (multiple windows, the shell, remote input injection), see
`docs/headless-testing.md` — the `TILO_TEST_CONTROL` socket, `--backend headless`,
and the `shots` binary. The perf harness here is the app-frame layer; that is the
system layer.
