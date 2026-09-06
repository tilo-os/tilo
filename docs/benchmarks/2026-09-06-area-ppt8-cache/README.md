# Area-PPT8 isolated driver-cache diagnostic

Status: all 12 predeclared launches are complete and audited; see RESULTS.md. Source remains frozen at Area-PPT8 v5. No default promotion or acceptance claim. The plan below was recorded before any launch.

Question: are the original initial-paint and sample-1 fence failures reproducible when a cache starts empty, and do subsequent launches using that same cache change the failure stage/completion? This tests a cache-sensitive first-use hypothesis, not an assumed diagnosis of shader compilation.

Two independent replications, each with separate empty default/PPT8 namespaces, three launches per namespace: 12 attempts in the exact order in plan.json. Reuse does not imply a fully warm cache, especially after a failed launch. All failures and incomplete runs remain in the denominator. Stop after the first two attempts if neither yields any nonempty cache file or cache-statistics evidence; preserve that invalid diagnostic rather than treating the environment variable as proof it worked.

Same preserved ordinary-release shell binary, 3840×2160, 1,000 actions, original warm-up/timers/one-second fence/four-attempt recovery/numeric gates. No CPU affinity, worker-count, shader, compiler, GPU query, workload or timeout changes. No other timed/build jobs. Each command limited to 15 minutes plus 30 seconds TERM grace. All experiment/profile variables cleared consistently; only PPT8 and per-command cache isolation/statistics vary.

Cache root created by mktemp: /tmp/qos-ppt8-cache-20260906.BaHsy5. Four separate child directories, initially empty. Normal user cache is neither cleared nor changed. All listed inherited cache controls were unset at preflight. Cache-format selection is left at Mesa's default; before/after snapshots record actual files, sizes and hashes. Stats may include probing/other driver instances: do not attribute aggregate hits to a particular fine shader without evidence. No cache files removed by this experiment.

Mesa package: mesa-vulkan-drivers-26.1.8-1.fc44.aarch64. The adapter must remain llvmpipe/Vulkan/Cpu as in the original experiment. Mesa documents the custom directory and termination hit/miss statistics in [environment variables](https://docs.mesa3d.org/envvars.html). Explicit wgpu pipeline-cache configuration and Mesa automatic disk caching are different layers.

The changed cache/statistics configuration makes every launch diagnostic-only, even if its existing numeric gate passes. This cannot establish the final performance goal or old-laptop/physical-GPU/60fps behavior. Source and preserved binary hashes are checked before/after; commands, raw outputs, exit codes, parsed reports, cache snapshots and all failures are retained.
