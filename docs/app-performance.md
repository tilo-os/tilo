# Application performance qualification

The [application inventory](../scripts/perf/app-suite.json) is the source of
truth for GUI entry points, benchmark hooks, proposed workloads, and current
qualification status. Inventory is coverage bookkeeping; an application is
qualified only when the evidence satisfies the contract below.

## Qualification contract

- Record the application, workload, build profile and executable digest, host
  configuration, viewport and scale, fixture size, warmup policy, and raw
  successful and failed samples. Missing or unsupported cases stay incomplete.
- Use optimized executables and verify their digest before and after a run.
- Measure at least 1,000 active-workload frames after warmup across repeated
  runs. At 60 Hz the interval budget is 16,666,667 ns; report p99 and every
  missed, skipped, dropped, or unavailable outcome.
- Exercise real scrolling, editing, resizing, animation, and applicable media or
  large-document paths. Verify the resulting state or pixels so ineffective
  input and stalled animation cannot pass.
- Keep CPU construction, render-worker work, GPU completion, and display
  presentation as separate populations. Do not add independent percentiles.
  CPU or `present()` duration alone does not prove displayed frame cadence.
- Treat startup and action completion latency as separate metrics. Preserve
  functionality, output quality, accessibility, security, responsiveness, and
  idle efficiency while optimizing.

## Shared tools

| Facility | Purpose and limit |
| --- | --- |
| [`ui::launch`](../crates/sdk/ui-core/src/launch.rs) and [`ui::headless`](../crates/sdk/ui-core/src/headless.rs) | Common lifecycle, input dispatch, bounded waits, and semantic assertions. Headless execution does not prove native presentation. |
| [`ui::profiling`](../crates/sdk/ui-core/src/profiling.rs) | Shared scenario steps and CPU frame capture. |
| [`ui::ux_bench`](../crates/sdk/ui-core/src/ux_bench.rs) | Structured input and semantic or visual checkpoints. Existing scenarios do not cover every application workload. |
| [`ui::offscreen`](../crates/sdk/ui-core/src/offscreen.rs) | GPU primitives and image checks. Offscreen completion is not display cadence. |
| [`runtime_benchmark`](../crates/session/shell/examples/runtime_benchmark/mod.rs) | Live Photos and Word Processor action workload. It covers only those applications and operations. |
| [`native-input-workloads.md`](native-input-workloads.md) | Scheduled native input, delivery, frame correlation, and effect validation. |
| [`native-presentation-feedback.md`](native-presentation-feedback.md) | Optional macOS Metal presentation observations and their limits. |

Application-specific flags and screenshot scenarios are useful fixture inputs,
but their existence is not performance evidence. Network, device, terminal,
remote-frame, and media applications need controlled providers and native
integration checks. OSK needs an input-method client, and Scratchpad needs its
own adapter because both intentionally bypass generic headless driving.

## Coverage workflow

1. Add representative fixture preparation and observable effect assertions.
2. Correlate active input IDs, CPU frame IDs, worker outcomes, and available
   display feedback without turning several inputs in one frame into several
   rendered frames.
3. Retain acquisition failures, skipped frames, occlusion, and background
   behavior as explicit outcomes.
4. Optimize a shared bottleneck only when controlled measurements locate it.
   Move to application-specific work when the remaining cost is application
   owned or shared experiments show no repeatable improvement.
5. Re-run correctness and qualification on every claimed host and architecture.

Run `python3 scripts/perf/check-app-coverage.py` from the repository root to
validate inventory coverage, package and binary names, exclusions, status, and
evidence references. The checker deliberately fails while applications remain
unqualified. It confirms that referenced files exist; it does not certify their
content or the performance claim. Focused checker tests are in
`scripts/perf/check_app_coverage_test.py`.
