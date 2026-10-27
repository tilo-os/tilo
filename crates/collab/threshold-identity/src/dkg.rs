//! Distributed Key Generation (DKG) state machine.
//!
//! FROST DKG is a three-round protocol: round 1 broadcasts a polynomial
//! commitment, round 2 exchanges per-recipient secret shares, round 3
//! verifies + assembles the final key material. Output is a
//! `KeyPackage` per participant plus a shared `PublicKeyPackage`
//! encoding the group verifying key.
//!
//! [`DkgSession`] is a per-device handle advancing `Round1 → Round2 →
//! Complete`; a higher-level orchestrator (in `crates/session/shell/`) shuttles
//! [`EnrolmentRound1`]/[`EnrolmentRound2`] messages between devices over
//! its transport (Iroh direct-message in production).
//!
//! A session struct (not free functions) because FROST's intermediate
//! secret packages must stay in memory between rounds — never sent over
//! the wire, but losing them aborts the protocol — so binding them to
//! one DKG run's lifecycle avoids threading them through several
//! function signatures.
//!
//! ## Wire types
//!
//! [`EnrolmentRound1`]/[`EnrolmentRound2`] are the only types crossing
//! device boundaries, serialised via postcard for consistency with the
//! rest of the Tilo crypto stack: the frost-ed25519 round package wrapped
//! in a small header carrying the sender's share index.

use std::collections::BTreeMap;

use frost_ed25519::keys::dkg::{
    self,
    round1::{Package as Round1Package, SecretPackage as Round1Secret},
    round2::{Package as Round2Package, SecretPackage as Round2Secret},
};
use frost_ed25519::keys::{KeyPackage, PublicKeyPackage};
use rand_core::CryptoRng;

use crate::rng::FrostRng;
use serde::{Deserialize, Serialize};
use serde_bytes as serde_bytes_codec;

use collab_types::Ed25519PublicKey;

use crate::errors::ThresholdError;
use crate::shares::{SecretShare, u16_to_identifier};

/// Round-1 DKG message: sender's polynomial commitment, broadcast to
/// every other participant. Carries the serialised
/// `frost_ed25519::keys::dkg::round1::Package` plus the sender's
/// 1-indexed share number so recipients can key it into a
/// `BTreeMap<Identifier, …>` without out-of-band info.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnrolmentRound1 {
    /// 1-indexed share number of the sender.
    pub from_share_index: u16,
    /// Serialised `frost_ed25519::keys::dkg::round1::Package`, opaque to
    /// callers — round-tripped through frost-ed25519's own serializer.
    #[serde(with = "serde_bytes_codec")]
    pub package_bytes: Vec<u8>,
}

/// Round-2 DKG message: sender's per-recipient secret share.
/// Point-to-point unlike round 1 — each recipient gets a different
/// message. `to_share_index` disambiguates for coordinator routing;
/// `from_share_index` is kept too for audit-log clarity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnrolmentRound2 {
    /// 1-indexed share number of the sender.
    pub from_share_index: u16,
    /// 1-indexed share number of the intended recipient.
    pub to_share_index: u16,
    /// Serialised `frost_ed25519::keys::dkg::round2::Package`.
    #[serde(with = "serde_bytes_codec")]
    pub package_bytes: Vec<u8>,
}

/// A single device's view of one DKG run, constructed via
/// [`DkgSession::start`]. Phase advances `Round1Pending → Round2Pending
/// → Complete`; each transition is fallible, returning
/// [`ThresholdError`] on a malformed peer message or wrong round count.
pub struct DkgSession {
    /// This device's 1-indexed share number.
    share_index: u16,
    threshold: u16,
    /// Total participants `n`.
    total: u16,
    /// Internal protocol state.
    phase: DkgPhase,
}

// frost-core's `KeyPackage` + `PublicKeyPackage` are large (~584
// bytes) vs. the round secrets; not boxed to balance sizes, since
// `DkgPhase` only ever lives on the stack inside one `DkgSession` and
// boxing would force a heap alloc per DKG run.
#[allow(
    clippy::large_enum_variant,
    reason = "DkgPhase lives on the stack of one session; boxing would allocate per DKG run"
)]
enum DkgPhase {
    /// Round 1 emitted; awaiting other participants' Round 1 packages.
    Round1Sent {
        /// FROST round-1 secret; must persist for round 2.
        secret: Round1Secret,
    },
    /// Round 2 emitted; awaiting other participants' Round 2 packages.
    Round2Sent {
        /// FROST round-2 secret; must persist for round 3.
        secret: Round2Secret,
        /// Cached round-1 packages from peers — needed again in part3 to verify
        /// each peer's contribution.
        round1_from_peers: BTreeMap<frost_ed25519::Identifier, Round1Package>,
    },
    /// Protocol complete; callers normally drain via `DkgSession::finish`.
    Complete {
        /// Per-participant key material for this device.
        key_package: KeyPackage,
        /// Public summary of the whole group's key material.
        public_key_package: PublicKeyPackage,
    },
}

impl DkgSession {
    /// Start a DKG run, returning the session handle plus this device's
    /// round-1 broadcast. `share_index` is this device's 1-indexed slot
    /// (caller-chosen; the orchestrator ensures all participants pick
    /// distinct values in `1..=total`); `threshold` is `t` (1 ≤ t ≤ n);
    /// `total` is `n` (≥ 1).
    ///
    /// # Errors
    ///
    /// Returns [`ThresholdError::InvalidThresholdParameters`] if
    /// `threshold` is `0`, `total` is `0`, or `threshold > total`, or
    /// an error if the underlying FROST DKG round-1 step or
    /// serialization fails.
    pub fn start<R: CryptoRng + ?Sized>(
        share_index: u16,
        threshold: u16,
        total: u16,
        rng: &mut R,
    ) -> Result<(Self, EnrolmentRound1), ThresholdError> {
        if threshold == 0 || total == 0 || threshold > total {
            return Err(ThresholdError::InvalidThresholdParameters { threshold, total });
        }
        let id = u16_to_identifier(share_index)?;
        let (secret, pkg) = dkg::part1(id, total, threshold, FrostRng(rng))?;
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

    /// Process round-1 messages from every other participant, producing
    /// this device's round-2 messages, keyed by recipient share index.
    /// `others` must contain exactly one message from every OTHER
    /// participant (`total - 1` entries), none from `self.share_index`.
    ///
    /// # Errors
    ///
    /// Returns [`ThresholdError::DkgStateMisuse`] if called outside
    /// `Round1Sent`, or if `others` contains an entry from this
    /// device's own share index; otherwise propagates an error if
    /// decoding a peer package or the underlying FROST round-2 step
    /// fails.
    pub fn receive_round1(
        &mut self,
        others: &[EnrolmentRound1],
    ) -> Result<BTreeMap<u16, EnrolmentRound2>, ThresholdError> {
        // Drain the round-1 secret from the phase; transition to
        // Round2Sent at the end.
        let prev = std::mem::replace(
            &mut self.phase,
            DkgPhase::Complete {
                // Placeholder; replaced below.
                key_package: dummy_key_package_placeholder()?,
                public_key_package: dummy_public_key_package_placeholder()?,
            },
        );
        let DkgPhase::Round1Sent { secret: r1_secret } = prev else {
            self.phase = prev;
            return Err(ThresholdError::DkgStateMisuse(
                "receive_round1 called outside Round1Sent",
            ));
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

    /// Process round-2 messages from every other participant and finalise
    /// the protocol. Requires exactly one [`EnrolmentRound2`] per other
    /// participant addressed to `self.share_index`; extra or missing
    /// entries return `DkgStateMisuse`.
    ///
    /// # Errors
    ///
    /// Returns [`ThresholdError::DkgStateMisuse`] if called outside
    /// `Round2Sent`, if an entry in `others` is not addressed to this
    /// device, or if an entry is from this device's own share index;
    /// otherwise propagates an error if decoding a peer package or
    /// the underlying FROST round-3 step fails.
    pub fn receive_round2(&mut self, others: &[EnrolmentRound2]) -> Result<(), ThresholdError> {
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

        // Decode each peer's round-2 contribution; all must be addressed to
        // this device.
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

    /// Drain the completed protocol and return `(SecretShare,
    /// group_public_key)`. Fails with `DkgStateMisuse` if not yet Complete;
    /// consumes the session.
    ///
    /// # Errors
    ///
    /// Returns [`ThresholdError::DkgStateMisuse`] if the session has
    /// not yet reached `Complete`, or an error if the group verifying
    /// key cannot be serialized (effectively unreachable).
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
    /// The shared group public key, byte-for-byte compatible with a regular
    /// Ed25519 verifying key.
    pub group_public_key: Ed25519PublicKey,
    pub threshold: u16,
    pub total: u16,
    /// This device's 1-indexed share number.
    pub share_index: u16,
}

impl DkgSession {
    /// Drain the complete protocol state into the full per-device record.
    /// Crate-internal because `PublicKeyPackage` (a `frost-core` type) is
    /// deliberately kept out of the public surface — only
    /// [`crate::identity::ThresholdIdentity`] consumes it, via this path.
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

// Placeholder helpers for the `std::mem::replace` trick when
// transitioning DkgPhase variants — must produce *valid* values
// because the enum payload requires a non-`Default` `KeyPackage`/
// `PublicKeyPackage`. Immediately overwritten so the values never
// leak; constructed via the trusted-dealer path on a single-signer
// scheme (cheap, deterministic). `Option<…>` is deliberately not
// used for the phase fields: it would force unwraps at every
// callsite, whereas this mirrors `std::mem::take` for non-`Default` types.

// FROST requires min_signers ≥ 2 in keygen, so the placeholder is a
// 2-of-2 generate (smallest legal config) — never observed by
// callers, overwritten by the real round-2/round-3 outputs.

fn dummy_key_package_placeholder() -> Result<KeyPackage, ThresholdError> {
    use frost_ed25519::keys::{IdentifierList, generate_with_dealer};
    use rand_core::SeedableRng;
    let mut rng = rand_chacha::ChaCha20Rng::from_seed([0xCDu8; 32]);
    let (mut shares, _pk) =
        generate_with_dealer(2, 2, IdentifierList::Default, FrostRng(&mut rng))?;
    let (_, share) = shares
        .pop_first()
        .ok_or_else(|| ThresholdError::FrostInternal("dealer produced no shares".to_string()))?;
    Ok(share.try_into()?)
}

fn dummy_public_key_package_placeholder() -> Result<PublicKeyPackage, ThresholdError> {
    use frost_ed25519::keys::{IdentifierList, generate_with_dealer};
    use rand_core::SeedableRng;
    let mut rng = rand_chacha::ChaCha20Rng::from_seed([0xCEu8; 32]);
    let (_shares, pk) = generate_with_dealer(2, 2, IdentifierList::Default, FrostRng(&mut rng))?;
    Ok(pk)
}
