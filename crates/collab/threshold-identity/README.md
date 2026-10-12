# tilo-threshold-identity

FROST(Ed25519, SHA-512) threshold-shared identity for Tilo.

## Headline property

A signature produced by collaborative threshold signing across
multiple devices verifies **byte-for-byte** through the standard
Ed25519 verification routine. The rest of the Tilo stack — including
`tilo_collab_types::verify_event` and the eight-step inbound-op
validator in `tilo_collab_types::validator` — does not need to know
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
  errors.rs          — `ThresholdError` (single error type)
tests/
  single_device.rs                — 1-of-1 bootstrap, sign, verify
  dkg_two_devices.rs              — 2-of-2 DKG + threshold sign
  dkg_three_devices.rs            — 2-of-3 DKG + sign with any 2
  recovery_share.rs               — paper-share export / re-import
  interop_with_collab_types.rs    — verify via tilo_collab_types
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
output by design. The first DKG run rotates the group public key.

## Wire types

`EnrolmentRound1`, `EnrolmentRound2`,
`SigningRound1Commitment`, and `SigningRound2Partial` derive
`Serialize + Deserialize` so the Tilo broker can ship them over
Iroh direct-message tunnels. Each carries a small Tilo header
(sender's share index) wrapping the FROST round-package bytes.

## Security notes

- `SecretShare` does not implement `Serialize`. Its `Debug`
  prints `<redacted>`. `frost-core`'s `KeyPackage` and
  `SigningKey` are `ZeroizeOnDrop`; the secret material is
  wiped on drop.
- Every public sign helper documents whether it canonicalises
  the input via `tilo_collab_types::canonical_bytes` (the
  `sign_round2` family) or signs raw bytes (`sign_round2_raw`).
  The canonical path is what makes the signature
  interchangeable with `tilo_collab_types::sign_event` over the
  same logical value.
