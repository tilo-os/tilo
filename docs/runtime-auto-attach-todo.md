# Automatic runtime attachment — todo

- [x] Confirm all-app scope and shared startup ownership.
- [x] Specify background runtime, inherited connection precedence and MCP attachment.
- [x] Implement automatic startup/join for normal framework launches; commits `c2d6045c8`, `6dce082e8`, and `2acfd7d8c`.
- [x] Focused verification: 14 bootstrap, 32 launch, 36 runtime, and 8 proof-harness tests passed.
- [x] Independently review implementation and fix both findings; `6dce082e8`, 13 bootstrap / 32 launch / 36 runtime tests pass.
- [x] Prove literal separate `cargo run` launches for Writer, Files, Photos, and late Calculator share one runtime/MCP; Calculator action reached `succeeded`, Writer exit left all other processes live, and exact cleanup completed (`target/runtime-proof/runtime-auto-attach/native-3`).
- [x] Update setup documentation with ordinary joining, daemon/profile lifecycle, developer bootstrap, inherited qOS behavior, explicit assistant `--connect`, and platform limits.
- [x] Complete whole-change review and final acceptance audit; configured-target bootstrap fixed in `2acfd7d8c`, scoped final review clean.
