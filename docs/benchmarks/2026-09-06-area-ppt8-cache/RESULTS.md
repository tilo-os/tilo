# Isolated cache diagnostic: results

All 12 predeclared launches completed as processes: six full 1,000-action reports and six fence failures. No timeout, warm-up, recovery, workload or code changes. Every launch remains diagnostic-only. All full reports fail the unchanged numeric gate. All 12 saved raw logs exactly match the captured output. All 33 source hashes and the preserved executable hash match preflight.

|Replication|Shader|Launch|Outcome|Exact mean ms|p99 ms|
|--|--|--:|--|--:|--:|
|1|default|1|initial-paint timeout|—|—|
|1|ppt8|1|initial-paint timeout|—|—|
|1|default|2|1,000 actions|41.609131|103.495|
|1|ppt8|2|sample-1 timeout|—|—|
|1|default|3|1,000 actions|40.662539|108.429|
|1|ppt8|3|1,000 actions|39.857287|115.713|
|2|ppt8|1|initial-paint timeout|—|—|
|2|default|1|initial-paint timeout|—|—|
|2|ppt8|2|sample-1 timeout|—|—|
|2|default|2|1,000 actions|43.395316|110.788|
|2|ppt8|3|1,000 actions|38.514870|98.546|
|2|default|3|1,000 actions|40.004917|115.943|

Both independent replications show the same failure-stage sequence:

- Launch 1, initially empty: both shaders fail initial paint. Both report two cache-statistic lines (0 hits/22 misses and 0 hits/8 misses), creating 31 files totaling about 2.25 MB.
- Launch 2, reused: the existing shader completes all actions; PPT8 fails measured sample 1. Both report 22 hits/1 miss and 8 hits/0 misses, and add one file.
- Launch 3, reused again: both shaders complete all actions and report 23 hits/0 misses and 8 hits/0 misses. Cache file counts and byte totals are unchanged from launch 2.

This reproducibly supports a cache-sensitive first-use cost. It exposes an existing cold initial-paint failure and an additional PPT8 sample-1 failure under these conditions. The location of that last cache miss plus the known first sparse action is consistent with deferred fine-shader compilation, but aggregate cache stats do not identify a shader or directly measure compiler time. Do not label that inference a proven compiler trace. Reusing cache removes these observed fence failures; it does not fix the high steady-state tails or prove universal stability. The two original failed comparison runs stay failed.

PPT8 remains opt-in. Do not promote it based only on its ordinary warm-cache speedup: peak RSS rose and this diagnostic exposed a repeated extra first-use failure. A production change must address the problem without hiding initial work, adding benchmark warm-ups, or extending fences.

Cache isolation and stats use Mesa's documented environment controls ([Mesa environment variables](https://docs.mesa3d.org/envvars.html)). The normal user cache was not deleted or changed by these launches. Four task-specific namespaces remain under /tmp/qos-ppt8-cache-20260906.BaHsy5; per-launch files/sizes/hashes are retained in the cache-before/cache-after JSON artifacts. Cache files are driver artifacts, not measurements of application GPU memory. Stats can include multiple/probing driver instances; no fine-shader-specific hit attribution is claimed.

These timings must not be mixed into the prior ordinary comparison: cache/statistics configuration differs, and captures happened later on the shared desktop. A post-run snapshot showed substantial lifetime CPU usage in desktop/app processes and load average 9.92 on eight CPUs, but that snapshot is not a per-frame concurrency trace or proof of any timing cause. No external process was stopped or reconfigured.
