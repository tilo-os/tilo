# perf: the session's performance, measured and held

What a person notices first is how long the session takes to be usable and
whether it stays smooth while it animates. Both are measured here, per device
class, and checked against budgets in `kpi-budgets.toml`.

| Harness | Measures | Runs on |
|---|---|---|
| `crates/session/desktop/examples/session_perf.rs` | the shared desktop surface (`arriving`: the native session's chrome; `apps`: the page's chrome and app paint): build, first frame, steady frame, typing frame, idle frames that did work, allocations per frame, peak RSS | any machine with the build toolchain: x86, the OPi, CI |
| `tools/perf/startup_marks.py` | a login's own startup timeline, from the session's marks to `first-frame-presented` | the device's session (`TILO_STARTUP_METRICS_PATH=<file>`) |
| `tools/perf/web_startup.mjs` | a page load in a browser: the page's marks, the wasm transfer, the heap after the first frame, and the frames presented while the strip pans | any machine with Chrome (headless) |
| `tools/perf/check_kpi.py` | compares any of those measurements with its budget | CI, a person |

A window's tile holds different content on the two hosts, so the frame costs
differ and are budgeted separately: the session gives a window's tab the
arriving placeholder and the window's own buffer covers it, while the page
mounts and draws the app in the tab.

Budgets are per device class, because the same millisecond is not the same
promise on the OPi 3B as on a laptop with a real GPU:

- `opi3b` — the floor: four Cortex-A76 cores, no GPU worth the name.
- `x86` — a desktop or laptop x86 machine with a real GPU.
- `web-desktop` — the web shell in a desktop browser.
- `web-opi` — the web shell in a browser on the OPi.

## Measuring

```sh
# The desktop frame path, on this machine (release: the frame cost without
# the debug profile's pessimism). Run both profiles: `arriving` is the
# session's frame, `apps` is the page's.
TILO_PERF_DEVICE=x86 cargo run -p tilo-desktop --release --example session_perf -- \
  --profile arriving --json x86-arriving.json
TILO_PERF_DEVICE=x86 cargo run -p tilo-desktop --release --example session_perf -- \
  --profile apps --json x86-apps.json

# A login on the device itself.
TILO_STARTUP_METRICS_PATH=~/session-startup.json tilo-session
tools/perf/startup_marks.py ~/session-startup.json --device opi3b --json opi3b-startup.json

# The web shell: build a bundle, then load it in headless Chrome.
crates/browser/tilo-browser-runtime/scripts/build.sh "$PWD/bundle" --bundle desktop
node tools/perf/web_startup.mjs --bundle "$PWD/bundle" --device web-desktop --json web.json
# The page names the graphics step it waits on (`device`, `pipelines`,
# `surface`), printed with its time: a slow first frame on a browser without
# a GPU is the browser compiling the renderer's pipelines, not the page.
# `--chrome-arg=SWITCH` passes one browser switch (`=`, since a switch starts
# with a dash), e.g. to ask for a hardware backend.

# Check every measurement against its budget.
tools/perf/check_kpi.py check x86-arriving.json x86-apps.json web.json
```

The first frame is a wall-clock promise, so it is measured with wall clock.
Frame time is measured through the production five passes (`sync`,
`build_layout`, `apply_layout`, `draw`, `handle_event`) with no GPU: it is the
CPU a frame owes, which is what a slow machine runs out of.

## Budgets

`kpi-budgets.toml` holds one table per device class and measurement kind. A
budget follows a measurement: set it from a run on the class, with the
headroom noted in its comment, and lower it when the measurement improves.
`test_check_kpi.py` proves the check fails when a budget is exceeded.

Steady-state allocations are budgeted at zero: a frame that allocates is a
frame that will stutter when the allocator does something other than hand back
a cached block.
