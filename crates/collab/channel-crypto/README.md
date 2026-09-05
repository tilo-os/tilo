# qos-channel-crypto

Per-channel MLS group encryption for qOS. Implements **Phase 7** of
the `2026-06-29-spaces-channels-and-cryptographic-roles.md` design
spec.

## What this crate does

Each qOS channel maps to one MLS group. The group key rotates on every
membership change. Application
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

Removal returns the commit every surviving member must process:

```rust
let removal_commit = alice_group.remove_member(bob_leaf_index)?;
carol_group.process_commit(&removal_commit)?;
```

The broker must durably fan out that commit before it reports removal as
complete. A removed member cannot process the removal commit or encrypt at the
new epoch.

## Production integration status

The production collaboration transport now accepts encrypted document
envelopes. The identity service and both local/network broker paths validate
the outer signature, current workspace role/capability, exact artifact and
attestation, and the deterministic route-derived MLS channel. They then store
and fan out the payload byte-for-byte without attempting to decode it. The UI
collaboration endpoint can be given a provisioned `ChannelGroup`; once
installed on a route, sends encrypt before signing and receives decrypt only
after broker validation. Missing, wrong, or future-epoch group state drops the
message rather than treating ciphertext as plaintext.

That is transport integration, not automatic key provisioning. `save_sealed` /
`load_sealed` persist the complete OpenMLS provider, signer, group id and epoch
under a caller-supplied vault key using authenticated XChaCha20-Poly1305 and
owner-only atomic replacement. `MlsGovernanceFrameV1` and
`GovernanceJournal` validate live membership revision/governor/epoch/hash-chain
metadata, atomically record opaque control bytes and exact recipients, replay
unacknowledged delivery after restart, and persist the plaintext-disable gate
only after a matching durable encrypted-snapshot acknowledgement.

The shell now has a broker-internal typed workspace-authority transaction that
accepts only registered vault/key-package handles and broker snapshot receipts,
writes an idempotent recovery intent, and couples the governance journal to
workspace capability admission/removal. It is deliberately not an app-facing
RPC. Production broker startup installs a Secret Service-backed MLS provider
under the launcher-supplied `XDG_STATE_HOME`; missing/locked Secret Service or
a missing state root reports unavailable rather than creating a local fallback
key. Consequently applications must not
claim that collaboration is E2E encrypted merely because the wire and endpoint
paths support it. Encrypted mode is active only after the trusted workspace
identity service supplies/restores the exact route's group.

## Remaining work

- **Production membership provisioning UX.** The versioned frame and journal
  implement targeted durable delivery. `add_member_transactional` and
  `remove_member_transactional` merge locally only after persistence succeeds.
  The shell coordinator supplies the authoritative revision and couples the
  write-ahead journal to role admission/revocation with crash recovery. The
  production broker installs the Secret Service implementation of
  `MlsVaultProvider` and its lifecycle coordinator; hermetic tests inject the
  same provider boundary. The authenticated peer control transport still needs
  to drain/acknowledge the journal's targeted outbox; the document relay must
  not be reused as an untargeted governance broadcast.
- **Late-join history backfill.** A new member starts at the
  inviter's current epoch; older ciphertexts they receive are
  dropped. Reading channel history before the join point lives in
  Phase 6 (compaction + lazy scrollback), not here.
- **Plaintext-history migration wiring.** Installing an MLS group makes that endpoint
  reject plaintext live and scrollback envelopes for the route, preventing a
  downgrade. Existing plaintext history therefore needs an explicit,
  authority-approved encrypted snapshot migration before MLS is enabled; no
  automatic migration currently exists. The durable gate requires a matching
  encrypted-snapshot acknowledgement. The authority boundary accepts only a
  broker-registered receipt and registers the resulting journal for ingress.
  Accepted encrypted compaction snapshots are fsynced into the provider's
  document store before it mints a route-bound, tamper-evident receipt.
- **Epoch rotation triggered by ops.** Welcomes happen explicitly via
  `add_member`. We do not pre-rotate keys on schedule.
- **External commit / external join.** Only Welcome-based joins.
- **Vault recovery ceremony.** Production custody is wired to the session's
  freedesktop Secret Service and never creates a key beside its ciphertext.
  Device recovery/export still needs product ceremony. Without the key, load
  fails explicitly and encrypted inbound traffic stays closed.
- **PCS / forward-secrecy proofs beyond openmls defaults.** We get
  what openmls gives us — sufficient for Phase 7's goals; harder
  claims need explicit analysis.
- **Multiple ciphersuites / negotiation.** One suite, fixed.
- **Direct-share (cross-channel single-recipient envelope).** That
  path is per-recipient HPKE, not MLS; not in this crate.

## Cipher suite

`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` — the canonical MLS
small-suite. Picked because:

- Ed25519 aligns with the rest of qOS (identity, FROST, Iroh).
- X25519 / AES-128-GCM / SHA-256 are universally implemented; no
  P-384 dependency for human-scale groups.
- Single fixed suite eliminates downgrade attacks and keeps the test
  surface small.

A future Phase 7.x can introduce a second suite under a versioned
handshake.

## Integration with `collab-types`

This crate ships two extensions to `collab-types`:

1. `SignedOp.encryption: Option<EncryptionMetadata>`. `None` means the
   payload is cleartext (legacy / unencrypted channels keep working);
   `Some { epoch, channel }` means the payload is a ciphertext from
   this crate's `ChannelGroup::encrypt`. The metadata IS covered by
   the outer signature so a peer cannot strip or rewrite it.

2. Legacy `ChannelEvent::MemberAdded { welcome, signed_by, signature }` and
   `ChannelEvent::MemberRemoved { removal, signed_by, signature }` are the
   existing signed governance carriers remain decode-compatible. New code uses
   `MlsGovernanceFrameV1`, which names the Welcome recipient, membership
   revision, post-change epoch, prior-frame digest, and the commit required for
   a 3+ member add. `GovernanceJournal` derives exact recipient outbox rows.

The signature pipeline (Phase 3) is unchanged — it still signs over
the canonical-bytes view of the envelope. The new `encryption` field
is part of that view.

## Version-pinning quirks

- **openmls 0.9** is the pinned line. The spec's brief mentioned
  0.6; the implementation has since moved to the 0.9 API with
  a usable API. The other companion crates pinned at this version:
  - `openmls_rust_crypto = "0.6"`
  - `openmls_basic_credential = "0.6"`
  - `openmls_traits = "0.6"`
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

The broker intentionally never decrypts. Encrypt-before-send and
decrypt-after-receive live at the application endpoint in
`ui::collaboration`; the broker validates and routes only signed clear
metadata. Automatic membership-driven provisioning and durable secret state
is not yet wired in the shell; the repository-native sealed state and durable
governance primitives it must call are implemented and restart-tested.

## Building

```bash
distrobox enter my-distrobox -- \
    bash -c 'cargo build -p qos-channel-crypto --release'
distrobox enter my-distrobox -- \
    bash -c 'cargo test -p qos-channel-crypto'
```

Both must run inside `my-distrobox`; the host lacks crypto build deps
in the standard qOS layout.
