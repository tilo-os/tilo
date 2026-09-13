# Acknowledged DKG Host Publication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development, one implementation author at a time. Steps use checkbox syntax. Revised after independent source/design preflight; implementation evidence remains required.

**Goal:** Keep a computed host DKG outcome retryable until its exact reply is durably acknowledged, and permit the pairer to recover from a lost broker response.

**Architecture:** Repair the existing PairingHandshake mailbox and AddDeviceHostScreen state ownership. Add a repeatable read to the existing authenticated, pinned TLS broker; preserve its legacy destructive read. Keep one exact host reply for the updated peek-based ceremony until expiry or cancellation. A legacy Take remains an explicit consumption exception; no lifetime tombstone or new acknowledgement store is added.

**Tech Stack:** Existing Rust, threshold-identity DKG, Postcard, std owned state/files, rustls broker and nextest.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md` identity/collaboration and reliability requirements. Source evidence: `.superpowers/sdd/2026-09-13-direct-shell-core-cutover/dkg-publication-diagnosis.md`.

## Global constraints

- Preserve exact certificate pinning, nonce/purpose authorization, transcript MAC, authority expiry/cancellation, private spool permissions and existing message/connection bounds.
- No mock DKG, increased existing test deadlines, increased pairer 15-second publication/reply budgets, relaxed assertions, or suite serialization as the repair.
- Host publication acknowledgement is neither peer completion nor secure persistence of the new host identity. Only the actual secret-store consumer may claim identity durability.
- Keep old Postcard request/response discriminants and legacy TakeHostRound2 consumption semantics. New requests append after existing index6.
- Exact conflict refusal applies while a packet is retained. Legacy Take removes it; a later put can admit a new value. Test this compatibility exception rather than claiming immutable lifetime history. Remote durable-acknowledgement claims require an upgraded authority, since the old broker falsely returned Ok on failed writes.
- Real Mac and exact-SHA Fedora focused tests, all-target checks/clippy and formatting are required. Do not read real user state or keys in tests.
- This slice repairs reusable enrollment logic. Production enrollment-screen/broker/secret-provider wiring and cross-host product walkthrough remain separate first-milestone obligations.

## Source-backed failure and limits

The observed full DKG test failed with host completion followed by a pairer 15-second missing-reply error; its existing retry passed. The source allows this when the64-yield durable lock acquisition fails and the void host publication drops that failure. The pairer's destructive polling performs writes even for an empty reply and can contend with publication. A lost TLS response after TakeHostRound2 also permanently consumes the only reply. No deterministic reproduction has yet run; the tests below must establish it before the repair.

## Task 1: Acknowledge exact publication and retain the computed outcome

**Files:** `ui/src/widget/identity_flow/{mod,add_device_host}.rs`, their existing unit modules; `shell/src/shell/identity_provisioning.rs` broker arm and tests. A focused private helper module beside identity_flow is permitted if it moves mailbox persistence logic without duplicating it.

**Consumes:** existing PairingHandshake backends/durable_update, real host_complete_dkg, PairingBrokerRequest::PutHostRound2 and PairingBrokerResponse::{Ok,Denied}.

**Produces:** existing publish method becomes acknowledged, following the established pairer-exchange bool convention:

```rust
pub fn publish_host_round2(&self, nonce: &[u8; 16], reply: EnrolmentRound2,
    mac: [u8; 64]) -> bool;
// true: exact packet+MAC is durably present (or stored by explicit memory fixture).
// false: no acknowledgement; caller must retain its packet and outcome.
```

The screen owns a private pending value, never cloned through a public state model:

```rust
struct PendingHostPublication {
    nonce: [u8; 16],
    reply: EnrolmentRound2,
    mac: [u8; 64],
    identity: threshold_identity::ThresholdIdentity,
    deadline: std::time::Instant,
}
enum HostPollState {
    Waiting,
    Pending(PendingHostPublication),
    Acknowledged,
    Terminated,
}
// AddDeviceHostScreen owns this private state; only successful begin resets it.
// poll_handshake tries that pending publication before taking another exchange.
```

- [ ] Build a real two-party DKG fixture using the existing AddDeviceHostScreen test setup. In a unit test's narrow private hook, acquire the actual nonce lock immediately after the real host DKG outcome is computed and before publication. Call the real poll path. Assert it returns false, reports waiting, exposes no completed identity, retains the exact packet and cannot recompute its consumed DkgSession. Release the lock; a later poll succeeds and both actual DKG outcomes share the same group public key. The hook holds a real opened create_new lock, not a fake publication return.
- [ ] Run `cargo nextest run --locked --profile ci --retries 0 -p ui --lib -E 'test(host_publication_)'`; record expected failing behavior. Add actual durable mailbox tests for exact retry, conflicting packet refusal, a failed directory sync after rename followed by retry, and corrupt state preservation. Fault injection applies only to named filesystem checkpoints, through the real serialization/update path.
- [ ] Implement exact retained publication in Memory and Durable backends. Compare the full packet and MAC (canonical serialized bytes are acceptable if the packet lacks equality); identical retries succeed only after establishing durable file and directory acknowledgement. Conflicts return false without rewriting the original bytes. Distinguish missing state from corrupt/unreadable/oversized state before mutation; never reset corrupt data to default in this strict path. Factor the one existing locked persistence algorithm with private `ReadPolicy::{Legacy,Strict}` and `UpdateDecision::{Persist,Reject}` (or an equivalent fallible update contract), then keep the old durable_update wrapper's deliberate compatibility behavior. Strict Reject aborts without serialization, rename or sync. Do not infer rejection by checking the file after a successful unrelated rewrite.
- [ ] Preserve compatibility for other mailbox callers while ensuring this strict path fails closed on expiry/cancellation before admission and again under the acquired lock. A directory-sync failure after rename returns false; a retry must perform the required synchronization, even if the matching value is already visible. A later empty/failed read cannot become a successful acknowledgement.
- [ ] Remote publication returns true only for an actual Ok response. The broker PutHostRound2 arm sends Ok only when the durable authority method returns true, otherwise Denied. Existing request/response positions do not change. Add an actual broker-response test proving a held durable lock cannot produce Ok and wrong nonce/cancel/expiry remain Denied.
- [ ] Extract a private DKG compute helper that consumes the engine without setting HostHandshakeState.completed. The poll path uses it and installs the pending reply/MAC/identity before publication. Preserve the public lower-level host_complete_dkg method's documented engine-completion behavior by delegating to that helper and setting its flag for direct callers; document this distinct contract. The screen poll path's flag stays false until acknowledged publication. The contention test asserts the public flag is false, the engine session is consumed, and no identity is available while pending.
- [ ] Make one publication attempt per existing poll tick, with no inner retry loop or UI-thread sleep. Successful publication moves identity into completed_identity, marks the poll state Acknowledged and session flag completed, then returns true exactly once. Acknowledged/Terminated polls do not consume another exchange or rerun DKG. Failed publication leaves Pending and returns false. Successful begin resets all state and discards any earlier pending/available identity; no old completion targets the new ceremony.
- [ ] Bound pending host publication to15 seconds from creation. This preserves the peer's independent existing15s window but does not guarantee an acknowledgement near the host deadline arrives before the peer deadline. Expiration moves to Terminated and returns a visible IdentityFlowError, with no available identity; only successful begin restarts. Invoke local invalidation on the actual cancel-button Back path, clearing pending/completed identity and disabling later polling even for the memory fixture whose backend cancel only clears its map. Drop releases owned state. Test cancel via the real event handler, terminal repeated polls, deadline through a private supplied-now helper, and fresh begin without sleeping. Do not log packet, MAC or secret key material.
- [ ] Run existing host DKG/MAC tests plus the new actual lock/sync/conflict tests and the existing full pinned-TLS DKG test. Self-review all publish_host_round2 callers and the pending-state transition, then commit `fix(identity): acknowledge and retain host DKG publication`. Controller reviews and verifies the exact SHA on Fedora before Task2.

## Task 2: Recover a lost host-reply response without destructive polling

**Files:** `ui/src/widget/identity_flow/{mod,pair_device}.rs`; `shell/src/shell/identity_provisioning.rs`; existing mailbox/broker tests.

**Consumes:** Task1 immutable acknowledged host reply and existing broker authorization.

**Produces:** a new repeatable operation on the same mailbox:

```rust
// Append to PairingBrokerRequest, after PutPairerExchange (index6):
PeekHostRound2([u8; 16]),
// PairingHandshake:
pub fn peek_host_round2(&self, nonce: &[u8; 16])
    -> Option<(EnrolmentRound2, [u8; 64])>;
// Reuse PairingBrokerResponse::HostRound2; legacy TakeHostRound2 stays unchanged.
```

- [ ] Add an instance-scoped test-only broker fault seam that closes one TLS connection after authorization/response computation but before bytes are written, only for PeekHostRound2 returning `HostRound2(Some(...))`. Empty polls cannot fire the fault. Test an actual pinned remote client: the first nonempty response is lost; the next authorized read returns the exact packet/MAC; a third remains identical. Use a bounded channel/barrier and assert the fault fired on the real stored packet. For behavioral RED, initially wire the new request/seam to the existing destructive operation, observe loss, then replace it with snapshot read. An absent API compile failure alone is insufficient. No global hook or new dependency.
- [ ] Implement read-only Memory/Durable/Remote peek using the existing mailbox. Durable peek uses the actual atomic file snapshot, performs no lock-file creation or write/fsync, and preserves current size/expiry/cancel checks. Response corruption/missing data remains unavailable, never an empty successful packet. Keep at most the one host reply already stored for that nonce, bounded by the existing ceremony and message limits.
- [ ] Add the append-only broker match arm with the same purpose/nonce/expiry checks as TakeHostRound2. Preserve encoded discriminants of all preexisting requests with concrete wire compatibility assertions; test the new request at index7. Unsupported older peers fail through the existing bounded unavailable/error path; do not silently fall back to destructive reads.
- [ ] Change the real pairer perform_pairing reply loop to peek_host_round2. Preserve the existing15s deadline,20ms cadence, transcript MAC verification and single receive_round2/adopt_dkg_outcome. A bad MAC fails before adoption. No acknowledgement deletes the retained reply; existing expiry/cancel governs its lifetime. Document this retention and avoid claiming new crash durability for the pairer's unpersisted DkgSession.
- [ ] Retain existing consumes-once tests for legacy TakeHostRound2, add peek/Take interaction and repeatable-read tests, and prove cancellation/expiry/refused nonce or invite-purpose requests cannot retrieve the packet. Empty peek leaves spool bytes, entries, sizes and modification state unchanged; access time may change on a filesystem read. Add the full real-DKG lost-response case through the nonempty broker fault seam, checking identical group public keys, an observed fault and no second host DKG computation.
- [ ] Run focused UI and shell identity suites, required checks/clippy/fmt and full native shell library after the final repair. Commit `fix(identity): retain host reply across broker response loss`; controller reviews and verifies the exact SHA on Fedora. Preserve any unexplained nextest LEAK/linker diagnostics as separate evidence, not a reason to weaken checks.

## Verification commands

All commands below are future obligations. On Mac prefix runtime commands with `PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py`; Fedora uses its established `/home/parallels/qos-target` and exact committed checkout. No shell source changes concurrently.

```sh
cargo nextest run --locked --profile ci --retries 0 -p ui --lib -E 'test(widget::identity_flow::)'
cargo nextest run --locked --profile ci --retries 0 -p shell --lib -E 'test(shell::identity_provisioning::)'
cargo check --locked -p ui -p shell --all-targets
cargo clippy --locked -p ui -p shell --all-targets -- -D warnings
cargo fmt --all --check
```

Use discovered names and fail on zero selected tests. The current native shell executable gate may require the separate native-runner task; report that exact prerequisite instead of calling a library check all-target success. New tests must be registered in actual compiled unit modules. Existing full pinned TLS DKG success is regression evidence, not proof of production screen/secret-store wiring or Mac↔Fedora product collaboration.

## Preflight resolutions

Independent report: `.superpowers/sdd/2026-09-12-macos-shell-build-foundation/dkg-publication-preflight.md`. Root accepted all five concrete corrections and the old-authority limit. Task1 now separates private compute from lower-level compatibility, explicitly models terminal polling/cancel and uses strict rejectable storage. Task2 drops a nonempty actual response, preserves legacy consumption as a qualified exception, and does not claim old authorities provide truthful acknowledgement. No source/test execution occurred during plan revision.

| Boundary | Producer/consumer check |
| --- | --- |
| Task1 | Real DKG computed once; private pending identity retained, public poll flag false until actual ack; terminal/cancel paths cannot reconsume. |
| Task1 storage | One shared lock/write/sync algorithm; Strict read distinguishes absent/corrupt; Reject has no effect; identical uncertain writes must sync successfully. |
| Task1 -> Task2 | Upgraded broker acknowledges the retained exact packet; updated peer reads it repeatably without deleting it. Legacy Take remains separately tested. |
| Task2 | Existing scoped TLS broker appends request7/reuses response2; fault drops only an authorized nonempty reply and must be observed by real DKG test. |
| Both tasks | Existing timing/authentication bounds preserved; one author/task review and exact-SHA both-host gates; product enrollment/secret storage remains separate. |
