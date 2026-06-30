# qos-channel-crypto

Per-channel MLS group encryption for qOS. Implements **Phase 7** of
the `2026-06-29-spaces-channels-and-cryptographic-roles.md` design
spec.

## What this crate does

Each qOS channel maps to one MLS group. The group key rotates on every
membership change (add today; remove in Phase 7.1). Application
payloads on the wire ride encrypted under the current epoch key; the
`SignedOp` envelope's metadata (author, artifact, op class, timestamp,
seq, plus the new `EncryptionMetadata`) stays cleartext so the broker
can route by artifact and filter by op class without decrypting.

The public surface is intentionally narrow:

```rust
use qos_channel_crypto::{ChannelGroup, MemberKey};
use qos_collab_types::ChannelId;

// Alice creates a fresh one-member channel.
let cid = ChannelId::new();
let alice = MemberKey::generate("alice")?;
let mut alice_group = ChannelGroup::create(cid, alice)?;

// Bob mints a KeyPackage and ships it to Alice out-of-band.
let bob = MemberKey::generate("bob")?;
let bob_kp = bob.fresh_key_package()?;

// Alice admits Bob. Welcome bytes ride a ChannelEvent::MemberAdded.
let welcome_bytes = alice_group.add_member(bob_kp)?;

// Bob joins from the Welcome.
let mut bob_group = ChannelGroup::process_welcome(cid, &welcome_bytes, bob)?;

// Both sides encrypt/decrypt.
let ct = alice_group.encrypt(b"hello bob")?;
let pt = bob_group.decrypt(&ct)?;
assert_eq!(pt, b"hello bob");
```

A 3+-member group uses `add_member_with_commit` + `process_commit`:

```rust
let (welcome, commit) = alice_group.add_member_with_commit(carol_kp)?;
bob_group.process_commit(&commit)?;        // bob advances epoch
let mut carol_group =
    ChannelGroup::process_welcome(cid, &welcome, carol)?;
```

## Out of scope (Phase 7 minimum)

- **Member removal.** `ChannelGroup::remove_member` is a documented
  `todo!()`. Phase 7.1 will wire the broker commit fan-out and the
  role-table `Revoke` op class.
- **Late-join history backfill.** A new member starts at the
  inviter's current epoch; older ciphertexts they receive are
  dropped. Reading channel history before the join point lives in
  Phase 6 (compaction + lazy scrollback), not here.
- **Epoch rotation triggered by ops.** Welcomes happen explicitly via
  `add_member`. We do not pre-rotate keys on schedule.
- **External commit / external join.** Only Welcome-based joins.
- **On-disk persistence.** All openmls state is in-memory; restart
  loses the group. Persistence is the broker's job in a later phase.
- **PCS / forward-secrecy proofs beyond openmls defaults.** We get
  what openmls gives us — sufficient for Phase 7's goals; harder
  claims need explicit analysis.
- **Multiple ciphersuites / negotiation.** One suite, fixed.
- **Direct-share (cross-channel single-recipient envelope).** That
  path is per-recipient HPKE, not MLS; not in this crate.

## Cipher suite

`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` — the canonical MLS
small-suite. Picked because:

- Ed25519 aligns with the rest of qOS (identity, FROST, libp2p).
- X25519 / AES-128-GCM / SHA-256 are universally implemented; no
  P-384 dependency for human-scale groups.
- Single fixed suite eliminates downgrade attacks and keeps the test
  surface small.

A future Phase 7.x can introduce a second suite under a versioned
handshake.

## Integration with `qos-collab-types`

This crate ships two extensions to `qos-collab-types`:

1. `SignedOp.encryption: Option<EncryptionMetadata>`. `None` means the
   payload is cleartext (legacy / unencrypted channels keep working);
   `Some { epoch, channel }` means the payload is a ciphertext from
   this crate's `ChannelGroup::encrypt`. The metadata IS covered by
   the outer signature so a peer cannot strip or rewrite it.

2. `ChannelEvent::MemberAdded { welcome, signed_by, signature }` and
   `ChannelEvent::MemberRemoved { removal, signed_by, signature }`.
   Governance events that the broker stores in the channel-event log
   and replays for joiners.

The signature pipeline (Phase 3) is unchanged — it still signs over
the canonical-bytes view of the envelope. The new `encryption` field
is part of that view.

## Version-pinning quirks

- **openmls 0.8** is the current stable. The spec's brief mentioned
  0.6, but `cargo search openmls` at landing time returns 0.8.1 with
  a usable API. The other companion crates pinned at this version:
  - `openmls_rust_crypto = "0.5"`
  - `openmls_basic_credential = "0.5"`
  - `openmls_traits = "0.5"`
  - `tls_codec = "0.4"`
- **`MlsMessageIn::into_welcome` is `#[cfg(test)]`-only in 0.8.1.**
  We extract the body via `MlsMessageIn::extract()` and pattern-match
  on `MlsMessageBodyIn::Welcome` instead.
- **`debug_assert!(false, ...)` on AEAD-tag mismatch.** openmls 0.8.1
  has a debug-build assertion in
  `framing/private_message_in.rs:136` that panics on any AEAD
  decryption failure. In release builds it returns the proper error.
  Our `decrypt` + `process_commit` wrap the openmls call in
  `std::panic::catch_unwind` so a malformed peer message never
  crashes the host. The wrapper can be removed once openmls fixes
  this upstream.
- **Ratchet tree must be inlined.** We pass
  `MlsGroupCreateConfig::builder().use_ratchet_tree_extension(true)`
  on group creation; without it the Welcome refers to a tree the
  joiner is expected to fetch out-of-band, which is heavier than we
  want for Phase 7's single-handshake-per-join model.
- **`KeyPackage` is re-exported from `openmls`** so callers can take
  / return key packages without pulling openmls into their own
  Cargo.toml. This couples our public API surface to openmls's minor
  versions; revisit if openmls reshapes the type in 0.9+.

## Honest end-to-end answer

> "If I encrypt a payload with this crate, ship it through the
> existing broker as a `SignedOp` with `EncryptionMetadata=Some`,
> and another node has the matching `ChannelGroup`, can they decrypt
> and apply?"

**Yes**, with the following constraints (all proven by the
integration tests in `tests/`):

- The recipient must have processed the corresponding
  `ChannelEvent::MemberAdded` (i.e. their `ChannelGroup` was joined
  from the Welcome embedded in that event).
- For 3+-member channels, the recipient must also have processed
  every `Commit` that advanced the group's epoch in between (so
  they're at the same MLS epoch or within openmls's
  out-of-order-tolerance window).
- The `SignedOp.signature` must verify (the outer envelope's bind
  to a qos-collab-types `Identity` + attestation).
- `qos-collab-types::validator::validate_inbound_op` must pass on
  the metadata (correct role, attestation, replay seq). Phase 3's
  pipeline runs *before* the decrypt; the decrypt runs only if all
  checks pass.

The **broker integration step** (encrypt-before-send,
decrypt-after-receive plumbing inside the broker process) is
explicitly NOT in this crate. That wiring is the next phase of work
on top of this foundation.

## Building

```bash
distrobox enter my-distrobox -- \
    bash -c 'cargo build -p qos-channel-crypto --release'
distrobox enter my-distrobox -- \
    bash -c 'cargo test -p qos-channel-crypto'
```

Both must run inside `my-distrobox`; the host lacks crypto build deps
in the standard qOS layout.
