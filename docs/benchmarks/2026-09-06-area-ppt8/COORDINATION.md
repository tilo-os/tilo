# Source and runtime coordination

Only the existing Terra geometry/vendor agent may edit the 11 allowlisted
Rust/WGSL files during this implementation. Root is preparing evidence scripts
and plans; no Cargo, GPU test or benchmark is running.

All new commands explicitly clear VELLO_AREA_PPT8 and the existing experimental,
profiling and benchmark override variables before applying the planned variant.
Release builds keep the project's ordinary release profile and target flags.
No ad hoc optimization level, CPU affinity, niceness, driver worker-count,
compiler target, AA mode, resolution or action-count change is part of this test.

PPT8 does not intentionally introduce persistent GPU buffers or CPU copies.
Its larger private per-invocation arrays may nonetheless increase driver
register pressure or compiler-generated scratch spilling. Existing known-GPU
accounting does not measure that private scratch; unchanged counters are not
proof of unchanged peak VRAM or physical memory traffic.

The 4x16-to-2x16 invocation shape also changes which X/Y coordinates may execute
together in a driver SIMD group. Branch divergence or lane locality could
offset interpreter/load savings; actual lane packing is not measured here.
Halving invocation count is therefore not a predicted 2x frame-time speedup.

The planned 30 attempts cover 18 shell and 12 UI cases in three counterbalanced
rounds. UI retains the dashboard_perf_bench subcommand and its native 4K/1,000
action settings. The initial plan draft omitted that subcommand and was
superseded by comparison-plan-v2.json before any execution.
