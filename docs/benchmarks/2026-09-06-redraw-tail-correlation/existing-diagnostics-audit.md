# Existing diagnostic audit (v5 source)

The source locations below refer to the verified XRGB v5 benchmark, before
the new diagnostic edit. No runtime or source change was performed by this audit.

- Per-action BenchmarkPhaseTiming is immediately reduced into phase totals
  (shell/examples/redraw_bench.rs, former lines 1216–1226 and 125–145).
- Wall samples are retained separately (former line 1255).
- GPU dispatch samples become independent per-stage vectors in
  GpuDispatchTimingTotals::record_completed_paint (former lines 630–705),
  and are printed as stage quantiles (former lines 912–960).
- No stored record joins the wall time, CPU phases and GPU timestamps of the
  same action. Comparing their independent p99 values cannot establish which
  phase produced a specific slow action.
- record_gpu_dispatch_timestamps already obtains the expected submission id
  and takes the existing completed/pending observation. A compact observation
  can be retained alongside existing aggregation without a new GPU query.
- BridgePhaseTiming map time includes rendered GPU work, appended readback
  copy, mapping setup and waiting. It is not pure execution time.
- Bridge CPU phases accumulate all bounded retries; the existing dispatch
  record retains only the final submission. Recovery counts must accompany
  each joined record, and coverage must not be misrepresented.
- Missing/stale/pending/invalid records must not be backfilled from adjacent
  actions or counted as exact-matched.
- Wall residual includes ordinary benchmark and diagnostic bookkeeping not
  represented by the phase fields. It is not automatically scheduling time.
- All Instant-based phase values are CPU-observed elapsed wall time, not
  per-thread CPU execution time. Submit includes renderer/driver calls and
  may overlap GPU work; map includes waiting. Do not add GPU stage durations
  to these wall-time phases as though they were disjoint costs.

Root checked the current stage catalogue: coarse IDs are 17, 24 and 28;
fine IDs are 20, 21, 22 and 23. Stage 7 (flatten), for example, is other
timed work, and stage 0 explicitly represents unknown/overflowed labels.
The join will also retain total/other/unknown time and coarse/fine dispatch
counts, not silently treat unreported stages as zero work.

Root selected all 1,000 bounded records rather than only the top 16 proposed
in the audit, preserving non-tail comparisons and missing-data denominators.
All new output is profile-only and emitted after the action loop. Diagnostic
overhead is not asserted to be zero or used as acceptance timing.

A separate read-only availability check found no perf executable on PATH;
perf_event_paranoid is 2. No profiler was installed, kernel setting changed,
or external profiling run attempted.
