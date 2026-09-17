# Automatic runtime attachment — todo

- [x] Confirm all-app scope and shared startup ownership.
- [x] Specify background runtime, inherited connection precedence and MCP attachment.
- [x] Implement automatic startup/join for normal framework launches; commit `c2d6045c8`, with independent review underway.
- [x] Initial focused verification: 10 bootstrap, 32 launch, and 33 runtime tests passed. Review will verify remaining edge cases before acceptance.
- [x] Independently review implementation and fix both findings; `6dce082e8`, 13 bootstrap / 32 launch / 36 runtime tests pass.
- [x] Prove literal separate `cargo run` launches for Writer, Files, Photos, and late Calculator share one runtime/MCP; Calculator action reached `succeeded`, Writer exit left all other processes live, and exact cleanup completed (`target/runtime-proof/runtime-auto-attach/native-3`).
- [x] Update setup documentation with ordinary joining, daemon/profile lifecycle, developer bootstrap, inherited qOS behavior, explicit assistant `--connect`, and platform limits.
- [ ] Complete the controller-owned whole-change review and final acceptance audit.
