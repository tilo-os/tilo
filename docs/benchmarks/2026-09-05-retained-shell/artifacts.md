# Artifact provenance

Source base: `3e03f98fd2b98ce9c86e1013126ae916bc3cbc81`, plus the retained-shell changes in this checkpoint.

Production build command:

```sh
cargo build -p ui -p shell --release --example ui-perf --example redraw_bench --locked
```

The build succeeded in 12m53s. It used the repository's normal release profile: opt-level 3, fat LTO, one codegen unit, panic abort, symbol stripping, line-table debug info, overflow checks off, incremental off. No RUSTFLAGS, target CPU, or release-profile overrides were supplied. Rust 1.97.1; host aarch64-unknown-linux-gnu; LLVM 22.1.6. The repository's x86-64-v3 target configuration is itself not compatible with a general 2010 x86 laptop; this checkpoint does not resolve that hardware requirement.

| Artifact (under ignored `target/ui-perf-comparison/`) | SHA-256 |
| --- | --- |
| `shell-redraw-retained-default-release-20260905` | `e18dff0cf8ff6e81ab971ee7557ee776979dfdd30ddde4fb4b1063e898d34ab9` |
| `ui-perf-retained-shell-default-release-20260905` | `23e69237f998ed531e5b055e696fde2b5420ca77ecb6f438744970808b8a2778` |
| Previous `shell-redraw-phase-profile-20260905` | `99f35976307b54a66bad9d8820fc2765a7cc0d39ca060766a5eff5bcdc4a42cb` |
| Previous `ui-perf-zero-gradient-default-release-20260905` | `11eecf86f82facc7c5002687f7398b4a4c1dc50b5e953554ad3762b243c3a1e0` |

The previous pair is preserved from commit `3e03f98fd`. Subsequent changes after the candidate release build are confined to test-only lint fixes, nextest scheduling, the standalone diagnostic example, and documentation. Production runtime behavior in the measured examples is unchanged.

The actual adapter identifies itself as `llvmpipe (LLVM 22.1.8, 128 bits)`, Vulkan, device type Cpu. These are native 3840×2160 software-rendered measurements, not physical GPU or 2010 laptop qualification. The shell harness includes full render completion/readback/XRGB output but does not measure DRM/KMS page flip or display scanout.

`retained_scene_probe` was built separately in the debug profile. Its four one-frame cold checks are diagnostic only and must not be compared with release latency gates.
