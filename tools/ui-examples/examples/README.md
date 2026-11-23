# UI example scenarios

The toolkit has three example executables. Scenario implementations remain in
separate source modules for focused review, but Cargo compiles and links only
these surfaces:

| Surface | Purpose | Command |
|---|---|---|
| `ui-lab` | Interactive component and state-machine catalog | `cargo run -p ui-examples --example ui-lab -- --list` |
| `ui-shots` | Deterministic screenshots and visual regressions | `cargo run -p ui-examples --example ui-shots -- --list` |
| `ui-perf` | Headless profiling and GPU benchmarks | `cargo run -p ui-examples --release --example ui-perf -- --list` |

Run a scenario by placing its name after `--`; any remaining arguments are
passed to that scenario:

```sh
cargo run -p ui-examples --example ui-shots -- history_cases target/shots-history
cargo run -p ui-examples --release --example ui-perf -- perf_scenario --count 240 --json
```

`registry.rs` owns the complete source inventory. Its tests reject duplicate
names and sources and scan this directory so a new scenario cannot be added
without assigning it to a surface.
