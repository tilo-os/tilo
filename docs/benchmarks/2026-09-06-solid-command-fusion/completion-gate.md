# Completion / polish gate

Do not commit or push based on diagnostic command counts or an opt-in win.
The user authorized polish, commit and push only after the performance goal
is achieved, and reserved the merge for themselves.

Required before calling the goal achieved:

- Exact requested pixels and all relevant regression/overflow/fallback checks.
- Ordinary release options; native3840x2160;1000 real actions per run.
- All fresh unflagged UI acceptance runs p99<=30000us.
- All fresh unflagged shell acceptance runs exact mean<=25000us andp99<=50000us.
- No replaced failed runs, dropped outliers, profiling or workload simplification.
- Clear reporting of CPU-software-Vulkan hardware limitation; no physicalGPU/
  2010-laptop/constant60FPS certification from these results.

Then polish only owned performance changes, removing unnecessary experiment
complexity where safe and preserving the benchmark evidence. Revalidate any
material post-measurement code change and rebuild/rerun acceptance if it can
affect timing. Inspect the dirty worktree and stage only exact owned paths;
security/OS/workflow edits belong to separate user work. Commit and normal
push the current branch only; no merge. CI status arrives automatically after
push. Investigate any relevant failure with available Forgejo tooling and
keep relevant CI green before the user merges.
