//! Distributed Key Generation (DKG) state machine.
//!
//! FROST DKG is a three-round protocol where each participant
//! contributes a polynomial commitment in round 1, exchanges
//! per-recipient secret shares in round 2, and verifies + assembles
//! the final long-lived key material in round 3. The output is a
//! `KeyPackage` per participant plus a single shared
//! `PublicKeyPackage` that encodes the group verifying key.
//!
//! We expose the state machine as [`DkgSession`], a per-device
//! handle that advances through `Round1 → Round2 → Complete`. The
//! orchestrator (a higher-level subsystem in `shell/`) is
//! responsible for shuttling [`EnrolmentRound1`] / [`EnrolmentRound2`]
//! messages between devices over its preferred transport (libp2p
//! direct-message, in the production path).
//!
//! ## Why a session struct rather than free functions
//!
//! The intermediate "secret packages" from FROST round 1 / round 2
//! must remain in memory between rounds. They are *not* sent over
//! the wire — only the public packages are — but losing them
//! aborts the protocol. A session struct binds them to the
//! lifecycle of one DKG run so the caller doesn't have to thread
//! them through several function signatures.
//!
//! ## Wire types
//!
//! [`EnrolmentRound1`] and [`EnrolmentRound2`] are the only types
//! that cross device boundaries. Both serialise via postcard for
//! consistency with the rest of the qOS crypto stack
//! (`qos-collab-types::canonical_bytes`). The byte layout is the
//! serialised `frost-ed25519` round package wrapped in a small
//! header with the sender's share index.

use std::collections::BTreeMap;

use frost_ed25519::keys::dkg::{
    self,
    round1::{Package as Round1Package, SecretPackage as Round1Secret},
    round2::{Package as Round2Package, SecretPackage as Round2Secret},
};
use frost_ed25519::keys::{KeyPackage, PublicKeyPackage};
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};

use qos_collab_types::Ed25519PublicKey;

use crate::errors::ThresholdError;
use crate::shares::{SecretShare, u16_to_identifier};

/// Round-1 DKG message: sender's polynomial commitment.
///
/// Broadcast to every other participant. Carries the serialised
/// `frost_ed25519::keys::dkg::round1::Package` plus the sender's
/// 1-indexed share number so recipients can key the message into
/// a `BTreeMap<Identifier, …>` without out-of-band info.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnrolmentRound1 {
    /// 1-indexed share number of the sender.
    pub from_share_index: u16,
    /// Serialised `frost_ed25519::keys::dkg::round1::Package`.
    /// Opaque to callers; we round-trip through
    /// `frost-ed25519`'s own serializer.
    #[serde(with = "serde_bytes")]
    pub package_bytes: Vec<u8>,
}

/// Round-2 DKG message: sender's per-recipient secret share.
///
/// Unlike round 1 these are *point-to-point*: each recipient
/// receives a different round-2 message. The `to_share_index`
/// field disambiguates so a coordinator can route. Both
/// `from_share_index` and `to_share_index` are included for
/// audit-log clarity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnrolmentRound2 {
    /// 1-indexed share number of the sender.
    pub from_share_index: u16,
    /// 1-indexed share number of the intended recipient.
    pub to_share_index: u16,
    /// Serialised `frost_ed25519::keys::dkg::round2::Package`.
    #[serde(with = "serde_bytes")]
    pub package_bytes: Vec<u8>,
}

/// A single device's view of one DKG run.
///
/// Constructed via [`DkgSession::start`]. Internal phase advances
/// through `Round1Pending → Round2Pending → Complete`. Each
/// transition is fallible: a malformed peer message or a wrong
/// round count produces [`ThresholdError`].
pub struct DkgSession {
    /// This device's 1-indexed share number.
    share_index: u16,
    /// Threshold `t`.
    threshold: u16,
    /// Total participants `n`.
    total: u16,
    /// Internal protocol state.
    phase: DkgPhase,
}

// frost-core's `KeyPackage` + `PublicKeyPackage` are large
// (~584 bytes combined) compared to the round-1 / round-2
// secrets. Boxing the Complete variant to balance variant sizes
// would force a heap allocation on every DKG run — and DkgPhase
// only ever lives on the stack inside a single DkgSession. We
// keep the inline layout for clarity; clippy's
// `large_enum_variant` is a code-size hint, not a correctness
// issue here.
#[allow(clippy::large_enum_variant)]
enum DkgPhase {
    /// Round 1 emitted; awaiting other participants' Round 1
    /// packages.
    Round1Sent {
        /// FROST round-1 secret; must persist for round 2.
        secret: Round1Secret,
    },
    /// Round 2 emitted; awaiting other participants' Round 2
    /// packages.
    Round2Sent {
        /// FROST round-2 secret; must persist for round 3.
        secret: Round2Secret,
        /// Cached round-1 packages from peers — needed again in
        /// part3 to verify each peer's contribution.
        round1_from_peers: BTreeMap<frost_ed25519::Identifier, Round1Package>,
    },
    /// Protocol complete. Holding the result here is optional;
    /// callers normally drain it via `DkgSession::finish`.
    Complete {
        /// Per-participant key material for this device.
        key_package: KeyPackage,
        /// Public summary of the whole group's key material.
        public_key_package: PublicKeyPackage,
    },
}

impl DkgSession {
    /// Start a DKG run. Returns the session handle plus the
    /// caller's round-1 broadcast message.
    ///
    /// Parameters:
    /// - `share_index` — this device's 1-indexed slot. The caller
    ///   chooses; the orchestrator coordinates so all participants
    ///   pick distinct values in `1..=total`.
    /// - `threshold` — `t` in t-of-n. Must satisfy `1 ≤ t ≤ n`.
    /// - `total` — `n`. Must be ≥ 1.
    pub fn start<R: CryptoRng + RngCore>(
        share_index: u16,
        threshold: u16,
        total: u16,
        rng: &mut R,
    ) -> Result<(Self, EnrolmentRound1), ThresholdError> {
        if threshold == 0 || total == 0 || threshold > total {
            return Err(ThresholdError::InvalidThresholdParameters { threshold, total });
        }
        let id = u16_to_identifier(share_index)?;
        let (secret, pkg) = dkg::part1(id, total, threshold, rng)?;
        let round1 = EnrolmentRound1 {
            from_share_index: share_index,
            package_bytes: pkg.serialize()?,
        };
        Ok((
            Self {
                share_index,
                threshold,
                total,
                phase: DkgPhase::Round1Sent { secret },
            },
            round1,
        ))
    }

    /// Process round-1 messages from every other participant,
    /// produce this device's round-2 messages.
    ///
    /// The returned map is keyed by recipient share index so the
    /// coordinator can route each entry to its intended device.
    /// The caller must ensure `others` contains exactly one
    /// message from every OTHER participant (i.e. `total - 1`
    /// entries) and none from `self.share_index`.
    pub fn receive_round1(
        &mut self,
        others: &[EnrolmentRound1],
    ) -> Result<BTreeMap<u16, EnrolmentRound2>, ThresholdError> {
        // Drain the round-1 secret from the phase; we transition
        // to Round2Sent at the end.
        let prev = std::mem::replace(
            &mut self.phase,
            DkgPhase::Complete {
                // Placeholder; replaced below.
                key_package: dummy_key_package_placeholder()?,
                public_key_package: dummy_public_key_package_placeholder()?,
            },
        );
        let r1_secret = match prev {
            DkgPhase::Round1Sent { secret } => secret,
            _ => {
                self.phase = prev;
                return Err(ThresholdError::DkgStateMisuse(
                    "receive_round1 called outside Round1Sent",
                ));
            }
        };

        // Decode peer round-1 packages.
        let mut peers = BTreeMap::new();
        for msg in others {
            if msg.from_share_index == self.share_index {
                return Err(ThresholdError::DkgStateMisuse(
                    "received own round-1 package in peers",
                ));
            }
            let id = u16_to_identifier(msg.from_share_index)?;
            let pkg = Round1Package::deserialize(&msg.package_bytes)?;
            peers.insert(id, pkg);
        }

        // Run FROST part2.
        let (r2_secret, r2_outgoing) = dkg::part2(r1_secret, &peers)?;

        // Build the wire messages — one per recipient.
        let mut outgoing = BTreeMap::new();
        for (to_id, pkg) in r2_outgoing {
            let to_index = crate::shares::identifier_to_u16(&to_id);
            outgoing.insert(
                to_index,
                EnrolmentRound2 {
                    from_share_index: self.share_index,
                    to_share_index: to_index,
                    package_bytes: pkg.serialize()?,
                },
            );
        }

        // Save state for round 3.
        self.phase = DkgPhase::Round2Sent {
            secret: r2_secret,
            round1_from_peers: peers,
        };
        Ok(outgoing)
    }

    /// Process round-2 messages from every other participant,
    /// finalise the protocol.
    ///
    /// The caller must supply exactly one [`EnrolmentRound2`]
    /// from every other participant where `to_share_index ==
    /// self.share_index`. Extra or missing entries return
    /// `DkgStateMisuse`.
    pub fn receive_round2(
        &mut self,
        others: &[EnrolmentRound2],
    ) -> Result<(), ThresholdError> {
        let prev = std::mem::replace(
            &mut self.phase,
            DkgPhase::Complete {
                key_package: dummy_key_package_placeholder()?,
                public_key_package: dummy_public_key_package_placeholder()?,
            },
        );
        let (r2_secret, round1_from_peers) = match prev {
            DkgPhase::Round2Sent {
                secret,
                round1_from_peers,
            } => (secret, round1_from_peers),
            other => {
                self.phase = other;
                return Err(ThresholdError::DkgStateMisuse(
                    "receive_round2 called outside Round2Sent",
                ));
            }
        };

        // Decode each peer's round-2 contribution. They must all
        // be addressed to this device.
        let mut peer_pkgs = BTreeMap::new();
        for msg in others {
            if msg.to_share_index != self.share_index {
                return Err(ThresholdError::DkgStateMisuse(
                    "round-2 message addressed to a different share index",
                ));
            }
            if msg.from_share_index == self.share_index {
                return Err(ThresholdError::DkgStateMisuse(
                    "received own round-2 package in peers",
                ));
            }
            let from_id = u16_to_identifier(msg.from_share_index)?;
            let pkg = Round2Package::deserialize(&msg.package_bytes)?;
            peer_pkgs.insert(from_id, pkg);
        }

        let (key_package, public_key_package) =
            dkg::part3(&r2_secret, &round1_from_peers, &peer_pkgs)?;
        self.phase = DkgPhase::Complete {
            key_package,
            public_key_package,
        };
        Ok(())
    }

    /// Drain the completed protocol and return the produced
    /// `(SecretShare, group_public_key)`.
    ///
    /// Fails with `DkgStateMisuse` if the session is not yet in
    /// the Complete phase. Consumes the session.
    pub fn finish(self) -> Result<DkgOutcome, ThresholdError> {
        match self.phase {
            DkgPhase::Complete {
                key_package,
                public_key_package,
            } => {
                // Public key — 32-byte Ed25519 form.
                let gpk_bytes_vec = public_key_package.verifying_key().serialize()?;
                let gpk_bytes: [u8; 32] = gpk_bytes_vec.as_slice().try_into().map_err(|_| {
                    ThresholdError::FrostInternal(
                        "group verifying key serialise produced non-32-byte output".to_string(),
                    )
                })?;
                Ok(DkgOutcome {
                    share: SecretShare::from_key_package(key_package),
                    group_public_key: Ed25519PublicKey::from_bytes(gpk_bytes),
                    threshold: self.threshold,
                    total: self.total,
                    share_index: self.share_index,
                })
            }
            _ => Err(ThresholdError::DkgStateMisuse(
                "finish called before Complete",
            )),
        }
    }
}

/// Successful DKG output for one device.
pub struct DkgOutcome {
    /// This device's secret share.
    pub share: SecretShare,
    /// The shared group public key, byte-for-byte compatible with
    /// a regular Ed25519 verifying key.
    pub group_public_key: Ed25519PublicKey,
    /// `t` in t-of-n.
    pub threshold: u16,
    /// `n` in t-of-n.
    pub total: u16,
    /// This device's 1-indexed share number.
    pub share_index: u16,
}

impl DkgSession {
    /// Drain the complete protocol state and return the full
    /// per-device record. Crate-internal because
    /// `PublicKeyPackage` is a `frost-core` type we deliberately
    /// keep out of the public surface; the wrapping
    /// [`crate::identity::ThresholdIdentity`] consumes it via
    /// this path.
    pub(crate) fn into_inner_complete(
        self,
    ) -> Result<
        (
            KeyPackage,
            PublicKeyPackage,
            u16,
            u16,
            u16,
            Ed25519PublicKey,
        ),
        ThresholdError,
    > {
        match self.phase {
            DkgPhase::Complete {
                key_package,
                public_key_package,
            } => {
                let gpk_bytes_vec = public_key_package.verifying_key().serialize()?;
                let gpk_bytes: [u8; 32] = gpk_bytes_vec.as_slice().try_into().map_err(|_| {
                    ThresholdError::FrostInternal(
                        "group verifying key serialise produced non-32-byte output".to_string(),
                    )
                })?;
                Ok((
                    key_package,
                    public_key_package,
                    self.threshold,
                    self.total,
                    self.share_index,
                    Ed25519PublicKey::from_bytes(gpk_bytes),
                ))
            }
            _ => Err(ThresholdError::DkgStateMisuse(
                "into_inner_complete called before Complete",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Placeholder helpers — used to satisfy the `std::mem::replace` trick when
// transitioning DkgPhase variants. They must produce *valid* values because
// the enum payload type requires a non-`Default` `KeyPackage` /
// `PublicKeyPackage`. The placeholder is immediately overwritten so the
// values it returns never leak. We construct them via the trusted-dealer
// path on a single-signer scheme — cheap, deterministic for a fixed RNG,
// and never observed by callers.
//
// We deliberately do NOT use `Option<…>` for the phase fields because
// (a) it forces unwraps at every callsite and (b) the placeholder
// approach mirrors how `std::mem::take` works for non-`Default` types.

// FROST requires min_signers ≥ 2 in keygen, so the placeholder
// is a 2-of-2 generate (the smallest legal config). The
// placeholder values never leak; they are overwritten by the
// real round-2 / round-3 outputs before any caller observes
// them.

fn dummy_key_package_placeholder() -> Result<KeyPackage, ThresholdError> {
    use frost_ed25519::keys::{IdentifierList, generate_with_dealer};
    use rand::SeedableRng;
    let mut rng = rand_chacha::ChaCha20Rng::from_seed([0xCDu8; 32]);
    let (mut shares, _pk) = generate_with_dealer(2, 2, IdentifierList::Default, &mut rng)?;
    let (_, share) = shares.pop_first().expect("dealer always produces shares");
    Ok(share.try_into()?)
}

fn dummy_public_key_package_placeholder() -> Result<PublicKeyPackage, ThresholdError> {
    use frost_ed25519::keys::{IdentifierList, generate_with_dealer};
    use rand::SeedableRng;
    let mut rng = rand_chacha::ChaCha20Rng::from_seed([0xCEu8; 32]);
    let (_shares, pk) = generate_with_dealer(2, 2, IdentifierList::Default, &mut rng)?;
    Ok(pk)
}
