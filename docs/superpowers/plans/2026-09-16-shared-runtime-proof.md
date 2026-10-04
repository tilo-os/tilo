# Shared runtime macOS proof

**Goal:** Run the real Photos and Word Processor executables concurrently on macOS, using the same production collaboration and identity brokers as Tilo, and record a typed cross-app content handoff.

**Design:** Add a compositor-free service host around `spawn_native` and the existing identity broker. Keep policy, certification, resource discovery, action dispatch and results unchanged. Isolate Unix descriptor inheritance in an adapter. The initial host links the shell library because the existing broker depends on its workspace model; this is deliberate reuse, not a second broker implementation or a claim that crate extraction is complete. No app-specific runtime integration is added.

**Constraints:** Use isolated identity, data and blob storage. Do not use harness trust bypasses. This proves trusted local native applications, not sandbox parity or a Windows port.

- [x] Add a regression test for isolated host lifecycle and exclusivity; observe failure before implementing the host.
- [x] Add `runtime_host::Session` using existing identity provisioning, workspace persistence, production collaboration and identity brokers. Add the Unix app-launch adapter with inherited service descriptors.
- [x] Add a proof executable that launches the actual Photos and Word Processor binaries, waits for both action surfaces, passes the image content reference published by Photos to `place-image`, checks results and app liveness, and emits machine-readable logs. Always reap its child processes.
- [x] Fix shared SDK issues exposed by the live run: identity lookups must share ownership/serialization of the inherited service, and an explicitly configured runtime content store must persist across processes. Add regressions first.
- [x] Build and run on macOS. Reconstruct the Word Processor document from signed broker events and verify the inserted image hash; retain runtime/app logs with PIDs, workspace, peer IDs, invocation IDs and content hash.
- [x] Run targeted existing broker tests and review the diff. Document evidence and remaining packaging limitations.

Verification commands: `cargo test -p shell --no-default-features --lib runtime_host_tests`; `cargo build -p shell --no-default-features --example shared-runtime-proof`; `cargo build -p photos --bin photos -p word-processor --bin word-processor`; `python3 scripts/shared-runtime-proof.py`.

The current Photos live document actions return no content product. The proof therefore uses the existing published image reference and existing insertion operation, avoiding new photo-editing semantics. This still exercises real cross-app discovery, invocation and content use through the shared runtime. Test evidence must not claim edited-image result chaining or model reasoning.

Completed macOS run: `target/runtime-proof/20260916-082747/events.jsonl`.
Runtime PID 28630; Photos PID 28661; Word Processor PID 28662; distinct broker
peers 0 and 1; insertion invocation 1 succeeded; two signed document events
verified; resulting image hash matched Photos' published hash. Both apps were
alive at verification and were reaped afterward.

Validation: 37 runtime-identity tests, 11 blob-store tests, one host lifecycle
test, and three production identity-broker tests passed. Clippy passed for
runtime-identity and qdoc libraries. A reviewer checked host descriptor lifetime,
proof validity, and the shared SDK fixes; its Command-reuse finding was fixed.
