# qos-threshold-identity

FROST(Ed25519, SHA-512) threshold-shared identity for qOS — the
cryptographic primitives for Phase 8 of the
`2026-06-29-spaces-channels-and-cryptographic-roles.md` spec.

## Headline property

A signature produced by collaborative threshold signing across
multiple devices verifies **byte-for-byte** through the standard
Ed25519 verification routine. The rest of the qOS stack — including
`qos_collab_types::verify_event` and the eight-step inbound-op
validator in `qos_collab_types::validator` — does not need to know
that an op was signed via threshold. This is the IETF RFC 9591
guarantee and it is exercised end-to-end in
`tests/interop_with_collab_types.rs`.

## Crate layout

```text
src/
  lib.rs             — public re-exports + crate-level docs
  identity.rs        — `ThresholdIdentity`: per-device handle, single
                       source of "sign / enrol / recover" methods
  dkg.rs             — Distributed Key Generation state machine
                       (`DkgSession`, `EnrolmentRound1/2`)
  signing.rs         — round-1 commit, round-2 sign, aggregate
                       (`SigningRound1`, `SigningRound2Partial`)
  shares.rs          — `SecretShare` newtype + `RecoveryShare`
                       paper-share encoding (BIP39 + metadata)
  transcript.rs      — pairing-transcript hash for QR-pair ceremony
  errors.rs          — `ThresholdError` (single error type)
tests/
  single_device.rs                — 1-of-1 bootstrap, sign, verify
  dkg_two_devices.rs              — 2-of-2 DKG + threshold sign
  dkg_three_devices.rs            — 2-of-3 DKG + sign with any 2
  recovery_share.rs               — paper-share export / re-import
  interop_with_collab_types.rs    — verify via qos_collab_types
```

## Three identity modes

```text
bootstrap_single_device  →  1-of-1    (SigningKey, plain Ed25519)
        +second device
DKG 2-of-2               →  2-of-2    (FROST, group_pk rotates)
        +third device
DKG 2-of-3               →  2-of-3    (FROST, group_pk rotates)
```

FROST keygen requires `min_signers ≥ 2`, so 1-of-1 mode stores a
plain `frost_ed25519::SigningKey` instead of going through DKG —
the signature it produces is byte-compatible with the threshold
output by design. The first DKG run rotates the group public
key; Phase 8 accepts that, refresh-based enrolment is a
forward-compatible upgrade.

## Wire types

`EnrolmentRound1`, `EnrolmentRound2`,
`SigningRound1Commitment`, and `SigningRound2Partial` derive
`Serialize + Deserialize` so the qOS broker can ship them over
libp2p direct-message tunnels. Each carries a small qOS header
(sender's share index) wrapping the FROST round-package bytes.

## Building this crate

The `[workspace]` block in `Cargo.toml` makes this crate a
**standalone** workspace. When merged back into the main qOS
workspace the orchestrator must strip that block and add
`qos-threshold-identity` to the root `Cargo.toml`'s
`[workspace] members = [...]`. Same precedent as
`qos-collab-types`.

In a worktree where the broader qOS workspace has unpopulated
submodules:

```bash
distrobox enter my-distrobox -- bash -c '
  cd .../qos-threshold-identity && cargo test'
```

## Tests

30 tests total. Notable:

- `interop_with_collab_types.rs::threshold_signed_op_passes_inbound_validator`
  — the grand-finale: a threshold-signed `SignedOp` passes the
  full eight-step `qos_collab_types::validate_inbound_op`
  pipeline, including the cryptographic signature check, with
  no special "threshold mode" plumbing.
- `recovery_share.rs::recovery_share_can_re_enter_quorum_after_device_loss`
  — disaster recovery: paper share re-enters a quorum after
  the original device is lost.

## Security notes

- `SecretShare` does not implement `Serialize`. Its `Debug`
  prints `<redacted>`. `frost-core`'s `KeyPackage` and
  `SigningKey` are `ZeroizeOnDrop`; the secret material is
  wiped on drop.
- Every public sign helper documents whether it canonicalises
  the input via `qos_collab_types::canonical_bytes` (the
  `sign_round2` family) or signs raw bytes (`sign_round2_raw`).
  The canonical path is what makes the signature
  interchangeable with `qos_collab_types::sign_event` over the
  same logical value.

## Out of scope (deliberate)

- No device-enrolment UI / QR pairing ceremony. The protocol
  primitives are here; the UX layer is a `shell/` + `ui/`
  follow-up.
- No persistence (`~/.config/qos/identity/`); the enrolment
  flow is responsible.
- No share refresh / proactive secret sharing. Each DKG
  produces a fresh group public key.
- No threshold *encryption* (MLS — Phase 7).
