# Native scheduled input diagnostics

Set `QOS_NATIVE_SCENARIO` to an absolute scenario JSON path to schedule native
input in `App::run_with_setup`. Set `QOS_PROFILE_OUT` to retain CPU frames and
`QUICK_UI_PRESENT=1` to retain render-worker outcomes. With no scenario, the
scheduler and its logging remain disabled.

```json
{
  "name": "button delivery",
  "interval_us": 16667,
  "warmup_steps": 1,
  "steps": [
    {"kind": "hover", "x": 100.0, "y": 30.0},
    {"kind": "click", "x": 100.0, "y": 30.0}
  ]
}
```

Coordinates are physical window coordinates. Supported steps include hover,
click, wheel line deltas, one Unicode scalar, and named keys. Keyboard steps use
the same constructor as headless driving. Unknown fields or keys, empty or
oversized timelines, non-finite values, invalid warmup, and overflowing
deadlines fail before scheduling. The scheduler begins after the first primary
worker `Presented` outcome, uses absolute deadlines, and does not shift future
deadlines after a late callback.

## Evidence model

`qos-native-input` records the input ID, requested deadline, scheduler-send
time, handler-entry time, and lateness. Report `scheduler_lateness` separately
from `delivery_to_dispatch`; their percentiles cannot be added.

`qos-native-frame` associates all inputs since the previous committed CPU frame
with the next CPU sample. Several inputs may share one frame, and an input may
produce no frame. Frames mixing warmup and measured input are excluded from the
measured CPU population. Worker rows carry the originating CPU frame ID where
available, allowing correlation without implying display presentation.

`scripts/perf/native_delivery.py` validates ordering, input coverage, payloads,
timelines, frame mapping, effect records, and numeric timing fields. It reports
the whole CPU/worker stream separately from measured-input frames. Fixtures
must independently verify state or pixels: delivery and frame correlation do
not prove that the intended operation happened or that the display met a frame
rate target.

Optional display feedback is described in
[`native-presentation-feedback.md`](native-presentation-feedback.md). It must be
joined by explicit frame identity. Input-to-presentation latency additionally
requires a proven clock relationship between scheduler timestamps and the
display API's host-time timestamps.

## Probe and application workloads

Build the native delivery probe with:

```sh
cargo build --profile thin-app -p ui --no-default-features --example ui-native-input-probe
```

The probe's button callback is useful for checking routing, coverage, and effect
counts. It is not a representative application workload. Application
qualification also requires fixture setup, named targets or stable coordinates,
semantic or pixel assertions, resize and pixel-wheel behavior where relevant,
and the actual engine or provider for browser, media, camera, and terminal apps.

When evaluating a renderer or admission change:

- compare repeated runs from the same source and optimized binary;
- retain failed and skipped outcomes rather than selecting a favorable run;
- separate scheduler delay, app dispatch, CPU construction, worker completion,
  and presentation intervals;
- measure idle behavior independently of active input;
- avoid attributing a change when instrumentation, window visibility, workload,
  or host activity also changed.

This machinery is diagnostic infrastructure. It does not by itself qualify any
application, host, or displayed frame rate.
