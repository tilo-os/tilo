# Benchmark executables

Ordinary locked release build completed successfully in 13m 28s, starting
2026-09-06 00:17:37 UTC. Source and configuration hashes in `build-inputs.txt`
were unchanged across validation and the build.

Command: `cargo build --release -p ui -p shell --example ui-perf --example redraw_bench --locked`,
with the same renderer/benchmark environment clearing listed in the plans.
No inherited Rust flags, release-profile, renderer/benchmark or graphics-driver
overrides were present.

Host: Linux aarch64, 8 online CPUs; rustc 1.97.1 (8bab26f4f, 2026-07-14),
cargo 1.97.1, LLVM 22.1.6. Workspace release settings retain opt-level 3, fat LTO,
one codegen unit, aborting panic, stripped symbols, disabled overflow checks,
line tables, and no incremental compilation. Native ARM target remains generic
with frame pointers. Both preserved artifacts are stripped ARM64 ELF PIE files.

```text
4c7a47a232ffb4f799cd28a73290cac12d98192f08c4ac39ff5a9dba46f1ce74  /home/user/qos/target/ui-perf-comparison/ui-perf-fine-quad-skip-20260906
adc01930d54024d094ae83912edf3c3815a4639e21756d5ba848bcc9da9593e1  /home/user/qos/target/ui-perf-comparison/shell-redraw-fine-quad-skip-20260906
```

No compiler or test remains running before the timed comparisons begin.
Candidate flags remain opt-in; this is not a default promotion or goal success.
The measurements do not certify physical 2010 hardware, KMS presentation,
physical GPU-bus traffic or constant 60 FPS.
