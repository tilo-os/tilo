# Initial reporting freeze review

Root verified that only shell/examples/redraw_bench.rs changed; the other
32 hashed v5 inputs are identical. No compilation/test/capture has run.

The initial implementation correctly uses the same captured elapsed value for
samples and the joined record, performs normal aggregate updates in their
existing timer position, and stores records afterward. The record vector is
created only for explicit profile mode and capped at 1,000. Exact submission
matching is kept distinct from missing/stale/pending observations; recovery
counts remain visible. Additional timestamp inspection is diagnostic overhead.

Two adjustments are requested before validation:

1. The verbose 1,000-row stdout report can exceed tool output limits. Add an
   optional profile-only QOS_REDRAW_BENCH_PROFILE_OUTPUT path. The benchmark
   creates it exclusively (no overwrite), writes/flushes after the loop, and
   propagates failures. Normal mode neither reads/allocates a path nor writes
   a report. Absent a path, stdout reporting remains available. Report-file
   mode prints a compact path/count acknowledgement, not duplicate rows.
2. Preserve total timed-stage, other-stage and unknown-stage ns plus
   coarse/fine dispatch counts. Coarse/fine alone hide other GPU work.
   Unknown-stage time is a subset of total and does not establish complete
   GPU execution coverage. Missing/unmatched samples remain absent, not zeros.

Additional focused tests should cover the writer and no-overwrite/error path,
off-mode gating, all explicit missing/status cases, and known-other/unknown
stage categorisation. Source is reopened only for these reporting fixes.

capture-plan-v2.json supersedes the initial capture plan before any runtime.
It changes only report artifact destinations and clears the new environment
key; workload, renderer flags, action count, ordering and limits are unchanged.
All four unique report paths are inside this evidence directory.
