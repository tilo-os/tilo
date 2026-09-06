# Opaque full-tile prune artifacts

Ordinary locked release build completed successfully in 13m 07s.
Host: Linux aarch64, 8 online CPUs; software Vulkan adapter is identified
in every raw benchmark log. No compiler/test process was live at batch launch.
No inspected Rust/profile/driver/thread overrides were set.
Source and build configuration hashes in build-inputs.sha256 matched before
validation and after release compilation. Artifacts were copied only after the
build completed, to previously nonexistent destination paths.

```text
40a5c8dfc8c3c19525276d6d60f66df27b927abd2e5255015345c788f19d1c2e  /home/xaver/qos/target/ui-perf-comparison/ui-perf-opaque-tile-prune-20260906
67198329609408b43399a722cc494ff53e3b9d1365c335fafa00bff30f2fac64  /home/xaver/qos/target/ui-perf-comparison/shell-redraw-opaque-tile-prune-20260906
```

One executable per workload is used across all four flag modes. Runtime
renderer and benchmark options are cleared by each stored invocation.

