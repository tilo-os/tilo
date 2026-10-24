# Tilo patch: davey on openmls 0.9

davey 0.1.5 builds on openmls 0.8 and its 0.5 providers. Their HPKE chain
(hpke-rs 0.6, libcrux-sha3 0.0.8) pins `hax-lib = 0.3.6`, while the
workspace's openmls 0.9 chain (and tilo-seal's hpke-rs 0.7) needs 0.3.7;
cargo cannot hold both, so davey moves to the generation the workspace
already builds. No released davey uses openmls 0.9 yet.

## `Cargo.toml`

- `openmls` 0.9, `openmls_basic_credential` 0.6, `openmls_rust_crypto`
  0.6; openmls's `js` feature (a wasm32 randomness switch) is dropped, as
  the voice bridge is native only.
- The optional Node (`napi`) and Python (`pyo3`) bindings are left out. Their
  feature names stay, without dependencies, so the `cfg_attr`s in the sources
  stay inert instead of tripping `unexpected_cfgs`.

## `src/session.rs`: an own commit is surfaced, not refused

openmls 0.8 failed processing one's own commit, fanned back by the voice
gateway as `dave_mls_announce_commit_transition`, with
`StageCommitError::OwnCommit`; davey caught that error and merged its pending
commit. openmls 0.9 instead returns `ProcessedMessageContent::OwnPendingCommit`
when the commit matches the pending one (and `OwnCommitMismatch` when it does
not), so `process_commit` merges the pending commit on that variant and
treats a mismatch like any other invalid commit. The worker's voice tests,
which run the whole DAVE handshake against davey sessions in the fake Discord,
cover it (`apps/discord/worker/tests/voice.rs`).
