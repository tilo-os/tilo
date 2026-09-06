# Current upstream main: benchmark plan

Completed: all 21 predeclared runs produced valid data. See [RESULTS.md](RESULTS.md) and the corrected [comparison-summary-v2.json](comparison-summary-v2.json). The tested upstream snapshot is31e5afdde; the remote advanced to84329100b during this task and that newer tip is not measured here. The original plan below is retained for provenance. No commit or push was performed.

User requested stopping commit/push, fetching current master and benchmarking it. No commit, push, merge, rebase or checkout of the original dirty working tree is authorized by this run.

- Upstream default branch: `main` (not `master`). Fetch completed 2026-09-06.
- Pinned upstream commit: `31e5afdde9e86e004b71635bf34d847520d4a5ba`.
- Isolated detached checkout: `/home/user/qos-main-benchmark-20260906.nKHtNP/checkout`.
- Original working tree: `/home/user/qos`; original tracked edits and new renderer source are hashed in `original-worktree-source.sha256`.
- No renderer optimizations, dependency changes, profile changes or shader changes will be ported into main. The benchmark adapter diff is preserved separately and must be reviewed before builds.

## Workloads and reporting

Main has no shell redraw benchmark. Its dashboard benchmark hardcodes 1800x1200 and includes screenshot readback. Minimal benchmark-only adapters will provide native 3840x2160, the original 1,000 alternating shell target clicks, and the animated 1,000-event / frozen one-million-event UI workloads. Native target RGBA payload is exactly 33,177,600 bytes. UI timing waits for GPU completion without a target screenshot readback; shell timing includes its production GPU readback and fused CPU XRGB write. Physical KMS scanout is not timed.

All main production renderer behavior stays at the pinned revision, including recovery behavior. The upstream shell bridge currently does not settle/check bump overflow; this is a correctness limitation and must not be silently fixed or counted as verified complete rendering. Workload/API/timing differences will be listed before interpreting results.

Build with the repository's ordinary locked release profile. No compiler, renderer, driver-thread, scheduling or cache-policy experiment. Use the same host and driver as branch captures. Driver shader caches remain in their existing state; do not delete user caches. Build and timed runs must not overlap.

Predeclare three attempts per main workload. For contemporary comparison, rerun the previously audited branch snapshot (clip v6 preserved binaries, all experiments off) in counterbalanced order. If the prior experimental Bounds+XRGB+PPT8 shell configuration is included, label it separately; never conflate it with defaults. Binary hashes, commands, environment controls, every failed attempt, timeout and outlier must be retained. Report medians across complete runs and every attempt's completion status, not just best runs. No runtime retry may replace a failed attempt. Time cap: 15 minutes plus 30 seconds TERM grace per command. Further build continuation after a cap must be explicitly recorded as a separate build attempt.

Full raw output goes directly to exclusive-created files through the recorder, avoiding tool-output truncation. Timing summaries are not emitted until source/binary provenance, sample counts and logs have been checked. This is the current aarch64 host with software Vulkan/llvmpipe, not a 2010 laptop or hardware-GPU/constant-60-FPS proof.

Status: the four benchmark adapters are frozen and all four added pure tests passed. Initial ordinary builds failed with malformed archives and then kernel-backed write I/O errors even when bypassing sccache. After the user authorized cache cleanup, the idle sccache server was stopped and its 14 GiB disposable cache was permanently removed; source, benchmark evidence, failed build targets and GPU shader caches were retained. Guest available space increased from 143 GiB to 156 GiB. A 1 MiB flushed write matched a direct-I/O SHA256 reread, with no new kernel warnings during cleanup/probe. This does not establish sustained storage health.

A controlled fresh-target build retry is recorded in build-plan-after-cleanup-v1.json. It reached the 15-minute cap (exit 124) without new storage errors; the observed Btrfs write-error counter stayed at 20,769. The separately recorded continuation in build-plan-after-cleanup-v2.json succeeded in 12m43s. All 14 frozen input hashes still match; all 942 new rlib headers are valid. Both flushed main executables matched their direct-I/O SHA256 rereads, and the complete build log matches its recorded byte count and hash. No compiler processes remain.

Runtime measurements use comparison-plan-after-cleanup.json and binaries-before.sha256. Only main binary paths changed across plan versions; workloads, controls and all 21 predeclared attempts are unchanged. Build/test memory is not application memory. The earlier performance goal remains unmet. No commit/push planned.
