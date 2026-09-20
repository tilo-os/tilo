# Shared runtime benchmark

Benchmark mode extends the shared-runtime acceptance proof with real Photos and
Word Processor actions and shared UI profiling. Run it from an active desktop:

```sh
cargo build -p shell -p photos -p word-processor --no-default-features \
  --example shared-runtime-proof --bin photos --bin word-processor
python3 scripts/shared-runtime-proof.py --benchmark
python3 -m unittest discover -s scripts/perf -p runtime_benchmark_test.py
```

For optimized measurements, build with `--profile thin-app` and pass
`--bin-dir target/thin-app`. Record the source revision and executable digest;
do not present development-build results as release performance.

The runner uses a disposable profile and retains `benchmark.json`, raw events,
per-app CPU frame streams, and render logs under `target/runtime-proof/`.
Evidence records the host, architecture, binary hashes, build profile, and
debug-assertion state. Benchmark windows must remain visible; missing render
streams fail the run rather than silently measuring an occluded app.

## Workload

The acceptance proof runs first. Benchmark mode then performs warmup and
measured Photos rotations and Writer image insertions. The Writer document grows
during the run, so report it as a growing-document workload. Each phase records
stream boundaries so startup and warmup samples stay outside measurement.

The shared resource profiler records declaration generation, semantic
construction, validation, permission projection, bound-resource work, control
refresh, and cache results. These spans locate work; they are not exhaustive
frame accounting. Logging can perturb the enclosing frame, generations are not
frame IDs, and independent stage percentiles must not be summed.

Worker rows carry an optional originating CPU frame ID. Reports retain unmatched
and unprofiled IDs and reject duplicates or malformed correlation fields. The
IDs correlate CPU construction with worker outcomes; they do not measure queue
latency or onscreen presentation.

## Reporting limits

Report startup, action discovery, action completion, broker start, CPU frame
construction, and worker stages as separate measurements. Include sample count,
minimum, maximum, mean, median, p95, and p99 with the raw population. Missing
streams, failed actions, incomplete cleanup, or an inconsistent binary fail the
report.

Desktop measurements include instrumentation and background load. Startup may
use warm filesystem caches. CPU and worker stages overlap, and neither a long
CPU frame nor a successful worker `present` call proves a dropped or displayed
frame. This benchmark does not measure input-to-photon latency, sustained FPS,
model inference, or per-action visual completion. Comparing runtime overhead
requires a matched baseline on the same host, build, and workload.
