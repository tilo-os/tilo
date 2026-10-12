# Live shared-runtime benchmark

Run from an active desktop session. This extends the existing two-process proof
and uses the production broker, real Photos and Word Processor actions, and
shared UI profiling hooks. No app-specific benchmark implementation is needed.

```sh
cargo build -p shell -p photos -p word-processor --no-default-features --example shared-runtime-proof --bin photos --bin word-processor
python3 scripts/shared-runtime-proof.py --benchmark
python3 -m unittest discover -s scripts/perf -p runtime_benchmark_test.py
```

For optimized measurements, build with `--profile thin-app` and pass
`--bin-dir target/thin-app` to the runner. That profile uses optimization level 3,
without LTO. Development results must not be presented as release performance.

The runner retains `benchmark.json`, raw events, per-app frame JSONL and render
logs under the printed evidence directory. It uses a disposable profile and
removes private identity material after stopping the apps. Evidence includes
the system, architecture, executable SHA-256 hashes, and debug-assertions flag.
The shared `TILO_PROFILE_WINDOW_POSITION=x,y` hook places Photos at logical
desktop position 20,60 and Word Processor at 840,60, each with the existing
800×600 logical window size. `TILO_PROFILE_WINDOW_FRONT=1` asks the window manager
to keep benchmark windows above unrelated desktop apps for the duration of the
run; ordinary app launches retain normal window ordering. Keep both windows visible on a sufficiently large
desktop. Window managers may ignore placement (notably Wayland); missing
render samples fail the run rather than silently benchmarking an occluded app.

Benchmark mode also enables `TILO_PROFILE_RESOURCES=1`. The shared UI emits
`tilo-resource-stage` JSON rows for declaration generation, semantic construction,
surface validation, permission projection, bound-resource work, control refresh,
and cache lookups. Reports group each stage by cache result, so cheap hits do
not dilute miss costs. Generations identify resource/permission state, not frame
IDs; these diagnostics include calls outside frame construction. Stage spans
exclude nested stages and their own serialization, but logging can perturb the
surrounding frame. Do not sum stage percentiles. Cache installation/eviction and
some temporary-value destruction are outside these stage spans. They locate
dominant work; they are not exhaustive frame accounting or a performance gate.

Worker rows now carry `cpu_frame_id`, the originating committed CPU sample ID,
alongside the independent worker `frame_ix`. Auxiliary/unprofiled frames use
null. Reports match explicit IDs, retain unmatched CPU/worker IDs, and count
unprofiled outcomes; duplicate IDs or missing correlation fields fail parsing.
This correlates outcomes only. Main/worker clock origins still differ, so these
fields do not measure queue latency or display presentation.

## Workload and timing boundaries

The initial proof verifies authorization, signed document events, and the actual
Photos content hash inserted into Word Processor. The benchmark then runs ten
warmup actions per app and fifty measured actions per app. Each pair rotates a
1280×720 photo right and inserts the original photo into Word Processor. Thus
the Word Processor document grows from 11 to 61 image blocks during measurement;
this is a growing-document workload, not repeated insertion into an empty file.

Each action is paced by 100ms after completion. Each phase has a 500ms settling
period before it and a 500ms tail before its end marker. Settling is a fixed
allowance, not a GPU fence or a guarantee of rendering completion. Phase markers
record completed line counts in each evidence stream, excluding startup and
warmup from frame statistics. Both workloads share the measurement window.

| Measurement | Start → end |
|---|---|
| Runtime startup | Before `Session::start` → returned session |
| Action surface observed | Before process launch → action found by 50ms polling; Word Processor observed after Photos |
| Action latency | Immediately before broker `start` → completion receiver returns |
| Broker start return | Same start → broker `start` returns an invocation job |
| CPU frame build | Existing shared profiler's frame-build timer; excludes later worker-channel send and rendering |
| Render stages | Existing `TILO_UI_PRESENT` CPU timers for acquire, encode, submit, and present API calls |

Action durations use monotonic `Instant` and retain nanoseconds; frame hooks
retain microseconds. Reports use milliseconds and nearest-rank median/p95/p99,
with sample count, mean, minimum and maximum. Fifty samples give limited tail
precision: p99 is the maximum. Missing frame/render streams, failed actions, or
incomplete runs fail the report instead of producing zero timings.

These are instrumented, desktop-session observations with profiling/log-write
overhead and possible background load. Startup is one observation with filesystem
caches potentially warm. CPU stages overlap across threads; their percentiles
must not be added. A long CPU frame build is not proof of a dropped display frame.
No GPU-completion, scanout, input-to-photon, sustained FPS, model inference, or
per-action visual-completion measurement is claimed. The benchmark measures the
full local action path, not an isolated runtime-overhead delta. Comparing runtime
overhead requires a matched baseline on the same hardware/build/workload.

## Measured development run, 2026-09-16

These numbers are an in-process diagnostic of a Cargo `dev` build. They time
the broker from inside the session. They do not include the control socket,
JSON (de)serialization or the compositor's event loop that an agent's call
goes through, so they are **not** the agent round-trip KPI. That KPI is
measured from the client's side of the control socket by
`scripts/perf/agent_rtt.py`. **Perf / scheduled** (`agent` group) runs it
nightly and keeps its records in the `tilo-perf-results` store ([docs/measurements/performance.md](measurements/performance.md)).

Evidence: `target/runtime-proof/20260916-083914/benchmark.json` and adjacent raw
streams. Apple M5 Max, 48 GiB RAM, macOS 27.0 arm64, Cargo `dev` build
(unoptimized application code; workspace overrides optimize some dependencies).
All 100 measured actions and 20 warmup actions succeeded. The signed cross-app
correctness proof passed before measurement. Four Python statistics tests and
the shared window-position unit test passed; benchmark review found no blockers.

| Metric | Samples | Median ms | p95 ms | p99 ms |
|---|---:|---:|---:|---:|
| Photos rotate action completion | 50 | 8.32 | 18.12 | 18.46 |
| Word Processor image insertion completion | 50 | 16.56 | 28.03 | 31.81 |
| Photos CPU frame build | 53 | 17.89 | 51.84 | 52.33 |
| Word Processor CPU frame build | 1440 | 0.97 | 1.40 | 16.31 |
| Photos worker encode | 53 | 1.74 | 2.12 | 2.93 |
| Word Processor worker encode | 43 | 1.54 | 1.81 | 3.42 |

Runtime startup took 81.4ms; action surfaces were observed around 1.35 seconds
after launch. These are single startup observations, not distributions.
Photos' sync pass accounted for most CPU frame-build cost (median 16.99ms,
p95 50.94ms). Of recorded CPU builds, 35/53 Photos and 14/1440 Word Processor
exceeded 16.667ms; these are not measured display-frame drops.

Word Processor recorded many cheap CPU rebuilds but only 43 successful worker
present calls within the measured window. CPU and render sample populations
differ and include unchanged/idle work; its low CPU p95 is not insertion-to-visible
latency. This run does not establish 60fps, release performance, or OS parity.
