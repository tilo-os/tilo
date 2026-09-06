# Benchmark executables

Built successfully with the ordinary locked release profile in 13m 53s.
Host: Linux aarch64; rustc 1.97.1, cargo 1.97.1. No inherited renderer,
benchmark, Rust flags, or release-profile overrides were present. Workspace
release settings retain opt-level 3, fat LTO, one codegen unit, aborting panic,
stripped symbols, disabled overflow checks, line tables, and no incremental
compilation. Native ARM target remains generic with frame pointers.

Source/configuration hashes in `build-inputs.txt` were unchanged across the
build. Both files below are preserved, not overwritten by subsequent builds.

```text
dfe10e11574ebcdabe34b5079c9335a05b9665ed5e48ad80d9743887df3d5f02  /home/user/qos/target/ui-perf-comparison/ui-perf-native-tile-bounds-20260906
dab82f97db353b8781bf1e775be05175833469dd88f6f5c62ca95f83ca74fe5c  /home/user/qos/target/ui-perf-comparison/shell-redraw-native-tile-bounds-20260906
```

No build or test overlaps a timed comparison run. These binaries still require
flags for the candidate; they are not a default promotion or goal completion.
