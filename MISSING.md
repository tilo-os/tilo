# MISSING.md

Honest accounting of what's built vs. what's actually engaged in the running system. As of `7402705` on `main`.

The foundation crates are good and the tests prove the pieces work in isolation. **What's missing is the integration spine.** Right now we have a real foundation under a notes app that uses approximately zero of it.

---

## 1. Every Phase past 2 is built but unused

The 2026-06-29 spec's nine phases shipped as types + primitives + tests. In the actually-running notes-app demo almost none of them fire:

### Phase 3 — Signed-wire enforcement

- Lives inside `shell::shell::collab_broker`.
- The deployed daemon is `apps/collab-broker` which uses the simpler `ui::collaboration::broker::serve_path` and fans plaintext frames.
- Apps still send `CollabRequest::SyncIncremental` not `CollabRequest::SignedSyncIncremental`.
- The eight-step `validate_inbound_op` pipeline runs in tests; in deployment it never executes.
- **Architectural fault line:** there are two collab-broker implementations and the wrong one is deployed.

### Phase 6 — Compaction + lazy scrollback

- `OpClass::Compaction` exists in `qos-collab-types`. No app emits one.
- No channel ever compacts. Scrollback is a feature without a trigger.
- `BrokerChannels::apply_compaction` + audit log path verified by tests, never invoked in practice.

### Phase 7 — MLS group encryption

- `qos-channel-crypto` crate works in isolation. 25 tests pass.
- The broker doesn't encrypt or decrypt payloads.
- Every CRDT increment between two collaborating notes apps is plaintext on the wire.
- `EncryptionMetadata` field on `SignedOp` is always `None` in practice.

### Phase 8 — Threshold identity

- `~/.config/qos/identity/primary.json` exists on disk for users, mode 0600, threshold-shareable.
- Nothing reads it. Notes uses `QUICK_APP_ID=alice` (a string) for presence attribution.
- No app signs an op with the persisted identity.
- The pairing-channel for device enrolment is an in-memory shim — two real devices cannot pair across the network. The DKG-over-broker plumbing is documented TODO in the source.
- Recovery share for single-device identities is a deterministic placeholder mnemonic; real recovery only works after upgrading to threshold mode (which the UI never triggers).
- `qos_threshold_identity::SecretShare::to_serializable_bytes()` doesn't exist as a public API, so the Phase 8 demo writes a placeholder secret to disk.

### Phase 9 — Cryptographic redaction

- Types + validator path + audit log entries exist.
- No UI surface. No app emits a `RedactRequest`. The compliance flow has no entry point.

### Phases 1, 4, 5 — UI widgets

- `ActivityTimeline`, `HistoryScrubber`, `ConflictBanner`, `ConflictPicker`, `ChannelBrowser` exist with full tests and `cargo run -p ui --example <name>` demos.
- None of them are bound to a real data source in any deployed app.

---

## 2. The architectural fault line

The deployed `apps/collab-broker` binary is a stripped 33-LOC wrapper around the legacy Plane-A relay in `ui::collaboration::broker`. The signed-wire / compaction / audit-log architecture I told you composes never composes in the running process because `shell::shell::collab_broker` is the production-shaped broker and it only runs inside the shell. Two paths exist; the wrong one is in production.

**Fix:** either lift `shell::shell::collab_broker` out of the shell crate into a shared `qos-collab-broker-core` crate and have `apps/collab-broker` use that, or extend `ui::collaboration::broker` to embed the same validation pipeline. The first is cleaner.

---

## 3. End-to-end gaps

- **No two-machine test has ever run.** Everything tested on localhost. `qos-relay` works in unit tests; no LAN deploy verified.
- **No systemd units for `collab-broker` or `qos-collab-link`.** They're foreground-only.
- **No broker federation.** Each broker is an island. The libp2p code lives in `shell` and doesn't reach the standalone daemon.
- **No xdg-desktop-portal / system-keyring integration for the on-disk identity.** It's plaintext JSON with `0600` as the only mitigation, called out as TODO Phase 8.5.
- **No cross-host invite flow.** The `qos://invite/...` URL parser works; nothing on the other side processes it into a `ChannelEvent::AttestationIssued`.

---

## 4. Desktop

The qOS shell renders apps that can't be interacted with. The whole compositor-side UX built widgets for has no working host:

- **Wheel disc** — implemented and renders, but workspace switching doesn't reflect real spaces/channels.
- **Channel-browser modal** as a chrome surface — widget exists, never opened by any shell action.
- **Audit-pane in chrome** — never shown.
- **Identity badge in topbar** — never shown.
- **Direct-shares inbox** — never shown.

The integration that turns these into something a user sees lives in `overlay_host.rs` and the chrome modules. None of it is wired. Apps work standalone via `cargo run -p notes`; "qOS as a desktop OS" doesn't.

---

## 5. User-facing entry points that don't exist anywhere

- No way to **create a Space or Channel** from a UI. Channels are constructs in `qos-collab-types`; no app exposes "new channel" anywhere.
- No way to **mint a role attestation** as an owner. Spec says "owners promote editors"; no button does that.
- No way to **view the activity timeline** of your work. The widget exists; no app shows it.
- No way to **file a RedactRequest.** The op class is defined; no command surface emits it.
- No **invite/accept flow** that actually delivers a member into a real channel.
- No way to **upgrade a single-device identity to threshold** (the prerequisite for a real recovery share to even be possible).

---

## 6. Text-editing gaps

- `TextInput` is single-line. Real soft newlines inside a paragraph render as separate blocks (Shift+Enter workaround). Long lines don't wrap.
- **Cross-paragraph copy / cut / paste don't work** — selection bands paint correctly across blocks, but Ctrl+C is captured per-TextInput and only sees the source block's slice.
- Selection survives the drag-to-Released boundary but not block restructuring (split / merge / typing). The cross-block drag state isn't carried through document edits.
- The Ctrl+Shift+Arrow user-side report (extending selection by word) was never pinned with a screenshot — relies on the user not seeing further issues.

---

## 7. Test-shape coverage gap

Every "everything composes" test is in-process — single binary spawns a broker, two `CollaborationManager`s, drives them through. **Zero tests cross a real socket boundary at the throughput-shaped layer.** Zero tests boot two real `notes` processes and verify a signed-encrypted op survives the link. Zero tests deploy `qos-relay` and prove cross-LAN.

---

## 8. Operational tooling gaps

- The `qos-collab-link` shaping daemon doesn't report stats anywhere (bytes/sec, queue depth, drop count). You can't tell if it's saturated.
- The broker doesn't expose Prometheus metrics, audit-log size, peer counts. Operations is blind.
- No way to **revoke a paired device** if it's lost — the threshold-identity refresh flow that preserves the group public key doesn't exist.
- No way to **rotate channel encryption keys** after a member is removed — Phase 7 ships member-add, member-remove is a `todo!()`.
- No way to **inspect a sandbox's permission grants** at runtime — the portal grants exist but no admin UI lists them.

---

## 9. Specific bugs / TODOs left in code

- `CursorLeft → Left` clears pointer state but **socket-close peer disconnect has no explicit propagation to subscribers** — a stale caret could linger forever in principle.
- The broker's `BrokerChannels` starts every channel with empty `authorised_signers` until `register_channel_state` is called. **In production no caller registers anything**, so every signed op would be rejected with `UnauthorisedAttestation`. The validator is dormant.
- `ChannelEvent` log replay is documented but **no broker replays event history on join.** Late joiners get a CRDT snapshot only.
- `recovery_share.rs::SerializableIdentity::from_identity` for threshold-mode identities is a stub — it deliberately can't extract the threshold secret share because `qos_threshold_identity` doesn't expose it. Phase 8.5 follow-up.

---

## 10. Off-spec but worth mentioning

- **No mobile / web clients.** Explicit in spec but should be flagged whenever a "join via QR" flow is described.
- **No accessibility tree integration** for the new identity-flow screens (AT-SPI / AccessKit). Main got `feat-a11y-accesskit`; identity-flow widgets ignore it.
- **No i18n** on the identity-flow screens — strings hardcoded English.
- **The wheel disc** in the shell renders workspaces, not Spaces. The Space/Channel model never crossed into shell state.
- **The collab-broker daemon ignores its identity** — multiple users on the same machine would share state via the same `/tmp` socket path.

---

## What the integration spine actually needs (concrete next steps)

1. **Lift broker validation out of `shell`** into a shared crate that both `apps/collab-broker` and the shell embed. The standalone daemon picks up Phase 3 / 6 enforcement for free.
2. **Wire `ui::CollaborationManager` to a real `SigningIdentity`** loaded from `~/.config/qos/identity/primary.json`. Switch the default outbound path from `SyncIncremental` to `SignedSyncIncremental`.
3. **Make the broker bootstrap `ChannelState` from a persisted channel registry** (TOML or Automerge) at startup so the role table isn't empty.
4. **Wire MLS encryption in the broker.** A channel-config bit (cleartext vs. MLS-encrypted) decides whether incoming/outgoing payloads get an encrypt/decrypt step.
5. **Add the channel-create / role-mint / invite-accept UI flows** somewhere — even a minimal admin pane in notes.
6. **Cross-paragraph copy/cut/paste in block_editor:** lift Ctrl+C / Ctrl+X / Ctrl+V to the BlockEditor level when a cross-block selection is active.
7. **Two-host e2e test:** systemd-spawn two VMs, run `qos-relay` between them, two notes processes each loading an identity, verify a signed-encrypted op crosses cleanly.
8. **Shell integration:** wire `overlay_host.rs` to actually display the audit / channel-browser / identity-badge surfaces; make `cargo run -p shell --mode hosted` produce a desktop that the user can interact with.

Steps 1–4 turn the dormant primitives into a functioning collaborative OS. Step 8 makes it look like the spec promised.
