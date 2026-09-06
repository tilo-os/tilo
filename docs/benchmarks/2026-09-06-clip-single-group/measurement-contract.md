# Planned ordinary-release comparison

This records the workload and comparison policy before measurements. Final executable paths, SHA-256 values, exact commands/order, and expected telemetry are recorded after source validation and an ordinary release build; no timing run is authorized yet.

Native 3840×2160, original 1,000 actions; unchanged warm-up, input, timers, damage/AA, fence, recovery and thresholds. Ordinary release settings with no inherited optimization/profile overrides. Clear all benchmark diagnostics/GPU timestamp flags and all unplanned renderer/cache overrides. Preserve existing default cache behavior. No other jobs while timed processes run. Each attempt has the existing 15-minute plus 30-second TERM cap. No extra warm-up or outlier removal.

Thirty attempts in three rounds:

- Shell: D, C, BX, BXC, BXP, BXPC (18 total).
- UI: D, C, P, PC (12 total).

D is default. C is VELLO_CLIP_SINGLE_GROUP=1. B is VELLO_SPARSE_COARSE_AUTO=1 plus VELLO_SPARSE_TILE_BOUNDS=1. X is QOS_DRM_XRGB_DAMAGE=1. P is VELLO_AREA_PPT8=1. Reverse each matched pair in round two and vary pair order in round three; exact order must be saved before execution. These are same-binary runtime comparisons, not cross-date comparisons to historical runs.

Why these controls: D/C measures the new clipping path alone; BX/BXC and BXP/BXPC measure it with the shell configurations that already improved mean/copies and then fine timing. UI D/C and P/PC separate clipping from the PPT8 fine candidate. No claim of UI bounds benefit: the preceding UI bounds-requested cohort had zero applied bounds.

Evidence eligibility requires the native extent, a full 1,000-action report, unchanged adapter, non-diagnostic configuration, safe positive statistics and consistent attempts/recovery/resource accounting. C-on must show requested/available true and positive actual C selected attempts; C-off must show zero. It is not required to select for every attempt: out-of-range clip counts correctly fall back. P-on/off, B-on/off, X-on/off must satisfy their existing selection/count/byte checks. Requested flags alone never prove a path was used. A no-selection cohort is retained but not called a speedup of that path.

Shell numeric gates remain exact sum/count mean <=25 ms and p99 <=50 ms. UI p99 <=30 ms. A numeric pass is not by itself complete acceptance: failures, first-use crashes and unavailable/no-selection attempts remain in the denominator. Report median complete-eligible timings with N, paired deltas and worst observed maxima/RSS; do not hide gate failures or compare unequal cohorts without warning. Keep CPU destination bytes separate from GPU copies/known resources/peak RSS/driver memory.

All attempts are retained, including timeouts and cold failures; no opportunistic replacement. Any instrumented follow-up is separately planned and cannot establish the goal. If candidate configurations pass repeatably, default promotion and a fresh no-opt-in verification are still required before polishing/committing/pushing. A hardware/constant60fps claim additionally requires evidence on the actual target; this host is llvmpipe/Vulkan/Cpu, not a 2010 laptop or hardware GPU.
