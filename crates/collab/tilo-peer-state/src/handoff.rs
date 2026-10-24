//! Handing a room's governance to another person's device: an ownership
//! transfer. A room's owner is its governor's person, so a new owner means a
//! new governor; the room keeps its id, its content and its history.
//!
//! Three statements, each signed by whoever makes it:
//!
//! 1. [`OwnershipOffer`]: the room's governor records that a device of the
//!    owner offered the room to a recipient. The owner device's
//!    authenticated request is the owner's confirmation; the offer lives in
//!    the room's governance ([`OWNERSHIP_OFFER_KEY`]) where the recipient
//!    and every member see it.
//! 2. [`HandoffAcceptance`]: a device of the recipient accepts that exact
//!    offer, signing over its own certificate and the transport hint of the
//!    host it will govern the room from.
//! 3. [`GovernorHandoff`]: the outgoing governor signs both into the room's
//!    final generation under its governance, logged under
//!    [`GOVERNOR_HANDOFFS_KEY`] together with the new owner. From then on the
//!    successor governs.
//!
//! A logged handoff is verified without the verifier's clock: one that was
//! valid when its governor committed it stays valid in the room's history,
//! whatever a member's clock says and whatever happens to the successor's
//! certificate later. Only the outgoing governor checks time, once, when
//! it accepts ([`check_acceptance`]).
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use ed25519_dalek::{Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;
use tilo_workspace_join::TransportHint;

/// The version every statement here carries.
pub const HANDOFF_VERSION: u16 = 1;
/// The governance key holding the pending offer, if any.
pub const OWNERSHIP_OFFER_KEY: &str = "ownership_offer_v1";
/// The governance key logging every committed handoff, by offer id.
pub const GOVERNOR_HANDOFFS_KEY: &str = "governor_handoffs_v1";
/// The longest an offer stays open.
pub const MAX_OFFER_NS: u64 = 7 * 24 * 3_600 * 1_000_000_000;

const OFFER_CONTEXT: &str = "tilo ownership offer v1";
const ACCEPTANCE_CONTEXT: &str = "tilo ownership acceptance v1";
const HANDOFF_CONTEXT: &str = "tilo governor handoff v1";

/// Why a handoff, or a step towards one, is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffError {
    NotOwner,
    ProposerCertificate,
    SuccessorNotRecipient,
    SuccessorCertificate,
    SameOwner,
    WrongOffer,
    AcceptanceSigner,
    GovernorSigner,
    NotCurrentGovernor,
    Replayed,
    Expired,
    FutureAcceptance,
    HintMismatch,
    RoomMismatch,
    UnsupportedVersion,
    /// The room was handed to another device, which governs it now: the
    /// device it was handed from changes nothing in it any more.
    Moved,
    Encoding(String),
}

impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOwner => f.write_str("only the room's owner offers it to someone else"),
            Self::ProposerCertificate => {
                f.write_str("the device that offered the room is not certified by its owner")
            }
            Self::SuccessorNotRecipient => f.write_str(
                "the accepting device is not a device of the person the room was offered to",
            ),
            Self::SuccessorCertificate => f.write_str(
                "the accepting device's certificate does not verify under its person's root",
            ),
            Self::SameOwner => f.write_str("the room is offered to its own owner"),
            Self::WrongOffer => {
                f.write_str("the acceptance is for another offer than the one the room has open")
            }
            Self::AcceptanceSigner => {
                f.write_str("the acceptance is not signed by the accepting device")
            }
            Self::GovernorSigner => {
                f.write_str("the handoff is not signed by the governor it hands the room from")
            }
            Self::NotCurrentGovernor => {
                f.write_str("the handoff is from a device that does not govern the room")
            }
            Self::Replayed => {
                f.write_str("the handoff was committed before and cannot be used again")
            }
            Self::Expired => f.write_str("the offer has expired"),
            Self::FutureAcceptance => {
                f.write_str("the acceptance is dated after the governor took it")
            }
            Self::HintMismatch => {
                f.write_str("the accepting device's host hint is not signed by it")
            }
            Self::RoomMismatch => f.write_str("the statement is about another room"),
            Self::UnsupportedVersion => f.write_str("unsupported handoff version"),
            Self::Moved => f.write_str("the room moved to its new owner's device"),
            Self::Encoding(error) => write!(f, "the statement does not encode: {error}"),
        }
    }
}

impl std::error::Error for HandoffError {}

fn digest(context: &str, value: &impl Serialize) -> Result<[u8; 32], HandoffError> {
    let bytes =
        postcard::to_stdvec(value).map_err(|error| HandoffError::Encoding(error.to_string()))?;
    Ok(blake3::derive_key(context, &bytes))
}

fn verify(key: &[u8; 32], digest: &[u8; 32], signature: &[u8; 64]) -> bool {
    VerifyingKey::from_bytes(key).is_ok_and(|key| {
        key.verify(digest, &ed25519_dalek::Signature::from_bytes(signature))
            .is_ok()
    })
}

/// A device's certificate, checked under its root with no clock: whether its
/// root signed it, never whether it is current.
fn certified(cert: &DeviceCert) -> bool {
    tilo_user_identity::verify_device_cert(cert).is_ok()
}

/// The owner's offer of `room` to `recipient`, as its governor records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipOffer {
    pub version: u16,
    pub room: String,
    /// Minted by the governor; never used twice in a room.
    pub offer_id: [u8; 32],
    /// The owner's person: the governor's user root key.
    pub owner: [u8; 32],
    /// The person the room is offered to, by user root key.
    pub recipient: [u8; 32],
    /// The owner's device that asked.
    pub proposed_by: DeviceCert,
    pub offered_at_ns: u64,
    pub expires_at_ns: u64,
}

impl OwnershipOffer {
    /// What an acceptance signs to name this exact offer.
    ///
    /// # Errors
    ///
    /// The offer does not encode.
    pub fn digest(&self) -> Result<[u8; 32], HandoffError> {
        digest(OFFER_CONTEXT, self)
    }

    /// Whether this offer is well formed for `room`, owned by `owner`.
    fn check(&self, room: &str, owner: [u8; 32]) -> Result<(), HandoffError> {
        if self.version != HANDOFF_VERSION {
            return Err(HandoffError::UnsupportedVersion);
        }
        if self.room != room {
            return Err(HandoffError::RoomMismatch);
        }
        if self.owner != owner || self.proposed_by.user_public_key != owner {
            return Err(HandoffError::NotOwner);
        }
        if !certified(&self.proposed_by) {
            return Err(HandoffError::ProposerCertificate);
        }
        if self.recipient == self.owner {
            return Err(HandoffError::SameOwner);
        }
        Ok(())
    }
}

/// A recipient device's acceptance of one exact offer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffAcceptance {
    pub version: u16,
    pub offer_digest: [u8; 32],
    /// The device that will govern the room.
    pub successor: DeviceCert,
    /// Where members reach the successor's host.
    pub successor_hint: TransportHint,
    pub accepted_at_ns: u64,
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct AcceptanceView<'a> {
    version: u16,
    offer_digest: &'a [u8; 32],
    successor: &'a DeviceCert,
    successor_hint: &'a TransportHint,
    accepted_at_ns: u64,
}

impl HandoffAcceptance {
    /// Accept `offer` as the device `successor`, whose key `key` is, to
    /// govern the room from the host `successor_hint` names.
    ///
    /// # Errors
    ///
    /// The statement does not encode.
    pub fn sign(
        offer: &OwnershipOffer,
        successor: DeviceCert,
        successor_hint: TransportHint,
        accepted_at_ns: u64,
        key: &SigningKey,
    ) -> Result<Self, HandoffError> {
        let mut acceptance = Self {
            version: HANDOFF_VERSION,
            offer_digest: offer.digest()?,
            successor,
            successor_hint,
            accepted_at_ns,
            signature: [0; 64],
        };
        acceptance.signature = key.sign(&acceptance.signing_digest()?).to_bytes();
        Ok(acceptance)
    }

    fn signing_digest(&self) -> Result<[u8; 32], HandoffError> {
        digest(
            ACCEPTANCE_CONTEXT,
            &AcceptanceView {
                version: self.version,
                offer_digest: &self.offer_digest,
                successor: &self.successor,
                successor_hint: &self.successor_hint,
                accepted_at_ns: self.accepted_at_ns,
            },
        )
    }

    /// Whether this acceptance accepts `offer`, made by a device of its
    /// recipient that signed it, with no clock.
    fn check(&self, offer: &OwnershipOffer) -> Result<(), HandoffError> {
        if self.version != HANDOFF_VERSION {
            return Err(HandoffError::UnsupportedVersion);
        }
        if self.offer_digest != offer.digest()? {
            return Err(HandoffError::WrongOffer);
        }
        if self.successor.user_public_key != offer.recipient {
            return Err(HandoffError::SuccessorNotRecipient);
        }
        if !certified(&self.successor) || self.successor.capabilities.foreign {
            return Err(HandoffError::SuccessorCertificate);
        }
        if self.successor_hint.host != self.successor
            || tilo_workspace_join::verify_transport_hint(&self.successor_hint).is_err()
        {
            return Err(HandoffError::HintMismatch);
        }
        if !verify(
            &self.successor.device_public_key,
            &self.signing_digest()?,
            &self.signature,
        ) {
            return Err(HandoffError::AcceptanceSigner);
        }
        Ok(())
    }
}

/// Whether the governor `governor` of `room` may record `offer`: the
/// owner's, from a device its root certified, to another person, open for
/// no longer than [`MAX_OFFER_NS`].
///
/// # Errors
///
/// Why the offer is refused.
pub fn check_offer(
    room: &str,
    governor: &DeviceCert,
    offer: &OwnershipOffer,
) -> Result<(), HandoffError> {
    offer.check(room, governor.user_public_key)?;
    if offer.expires_at_ns <= offer.offered_at_ns
        || offer.expires_at_ns.saturating_sub(offer.offered_at_ns) > MAX_OFFER_NS
    {
        return Err(HandoffError::Expired);
    }
    Ok(())
}

/// Whether the governor `previous` of `room` may take `acceptance` of
/// `offer`, its open offer, at `now_ns` by its own clock: everything a
/// logged handoff is checked for, plus the offer's expiry and an
/// acceptance not dated after now. Only the outgoing governor asks this.
///
/// # Errors
///
/// Why the acceptance is refused.
pub fn check_acceptance(
    room: &str,
    previous: &DeviceCert,
    offer: &OwnershipOffer,
    acceptance: &HandoffAcceptance,
    now_ns: u64,
) -> Result<(), HandoffError> {
    offer.check(room, previous.user_public_key)?;
    acceptance.check(offer)?;
    if now_ns >= offer.expires_at_ns {
        return Err(HandoffError::Expired);
    }
    if acceptance.accepted_at_ns > now_ns {
        return Err(HandoffError::FutureAcceptance);
    }
    Ok(())
}

/// The outgoing governor's statement that the room's governance passes to
/// the acceptance's successor, in the room generation `generation`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernorHandoff {
    pub version: u16,
    pub offer: OwnershipOffer,
    pub acceptance: HandoffAcceptance,
    /// The governor handing the room off.
    pub previous: DeviceCert,
    /// The room generation committing it: every later handoff of the room
    /// has a greater one.
    pub generation: u64,
    /// When the governor committed it, by its clock; never before the
    /// acceptance.
    pub committed_at_ns: u64,
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct HandoffView<'a> {
    version: u16,
    offer: &'a OwnershipOffer,
    acceptance: &'a HandoffAcceptance,
    previous: &'a DeviceCert,
    generation: u64,
    committed_at_ns: u64,
}

impl GovernorHandoff {
    /// The governor `previous`, whose key `key` is, hands the room off to
    /// `acceptance`'s successor in generation `generation` at
    /// `committed_at_ns`, its own clock. The acceptance's time is bounded by
    /// the governor's: it cannot be dated after the commit.
    ///
    /// # Errors
    ///
    /// The acceptance is dated after `committed_at_ns`, or the statement
    /// does not encode.
    pub fn sign(
        offer: OwnershipOffer,
        acceptance: HandoffAcceptance,
        previous: DeviceCert,
        generation: u64,
        committed_at_ns: u64,
        key: &SigningKey,
    ) -> Result<Self, HandoffError> {
        if acceptance.accepted_at_ns > committed_at_ns {
            return Err(HandoffError::FutureAcceptance);
        }
        let mut handoff = Self {
            version: HANDOFF_VERSION,
            offer,
            acceptance,
            previous,
            generation,
            committed_at_ns,
            signature: [0; 64],
        };
        handoff.signature = key.sign(&handoff.signing_digest()?).to_bytes();
        Ok(handoff)
    }

    fn signing_digest(&self) -> Result<[u8; 32], HandoffError> {
        digest(
            HANDOFF_CONTEXT,
            &HandoffView {
                version: self.version,
                offer: &self.offer,
                acceptance: &self.acceptance,
                previous: &self.previous,
                generation: self.generation,
                committed_at_ns: self.committed_at_ns,
            },
        )
    }

    /// The device governing once this handoff is followed.
    #[must_use]
    pub const fn successor(&self) -> &DeviceCert {
        &self.acceptance.successor
    }

    /// Whether this handoff, logged in `room`, hands it from `authority`,
    /// the device governing it, with every signature and certificate in
    /// place and `after` (the generation of the handoff followed last)
    /// behind it. No clock is read.
    ///
    /// # Errors
    ///
    /// Why the handoff is refused.
    pub fn verify(
        &self,
        room: &str,
        authority: &DeviceCert,
        after: u64,
    ) -> Result<(), HandoffError> {
        if self.version != HANDOFF_VERSION {
            return Err(HandoffError::UnsupportedVersion);
        }
        if self.previous != *authority {
            return Err(HandoffError::NotCurrentGovernor);
        }
        if self.generation <= after {
            return Err(HandoffError::Replayed);
        }
        self.offer.check(room, authority.user_public_key)?;
        self.acceptance.check(&self.offer)?;
        if self.acceptance.successor.device_public_key == authority.device_public_key {
            return Err(HandoffError::SameOwner);
        }
        if self.acceptance.accepted_at_ns > self.committed_at_ns {
            return Err(HandoffError::FutureAcceptance);
        }
        if !verify(
            &authority.device_public_key,
            &self.signing_digest()?,
            &self.signature,
        ) {
            return Err(HandoffError::GovernorSigner);
        }
        Ok(())
    }
}

/// The open offer `doc`'s governance records, if any.
#[must_use]
pub fn open_offer(doc: &AutoCommit) -> Option<OwnershipOffer> {
    match doc.get(ROOT, OWNERSHIP_OFFER_KEY).ok().flatten()? {
        (Value::Scalar(value), _) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => postcard::from_bytes(bytes).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Every handoff `doc`'s governance logs, in generation order. An entry that
/// does not decode is skipped: it hands nothing off.
#[must_use]
pub fn logged(doc: &AutoCommit) -> Vec<GovernorHandoff> {
    let Some((Value::Object(_), map)) = doc.get(ROOT, GOVERNOR_HANDOFFS_KEY).ok().flatten() else {
        return Vec::new();
    };
    let mut handoffs: Vec<GovernorHandoff> = doc
        .keys(&map)
        .filter_map(|key| match doc.get(&map, &key).ok().flatten()? {
            (Value::Scalar(value), _) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => postcard::from_bytes(bytes).ok(),
                _ => None,
            },
            _ => None,
        })
        .collect();
    handoffs.sort_by_key(|handoff| handoff.generation);
    handoffs
}

/// The handoffs of `handoffs` that hand `room` on from `authority`, the
/// device governing it, in order: each from the governor the one before
/// handed it to, each in a later generation. A handoff that does not verify
/// is not followed. Nothing here reads a clock.
#[must_use]
pub fn follow(
    room: &str,
    authority: &DeviceCert,
    handoffs: &[GovernorHandoff],
) -> Vec<GovernorHandoff> {
    // The handoff that made `authority` the governor, if one did: none
    // before it is followed again.
    let mut after = handoffs
        .iter()
        .filter(|handoff| handoff.successor() == authority)
        .map(|handoff| handoff.generation)
        .max()
        .unwrap_or(0);
    let mut governor = authority.clone();
    let mut steps = Vec::new();
    for handoff in handoffs {
        if handoff.verify(room, &governor, after).is_ok() {
            governor = handoff.successor().clone();
            after = handoff.generation;
            steps.push(handoff.clone());
        }
    }
    steps
}

/// The handoffs `doc` logs that hand `room` on from `authority`
/// ([`follow`]).
#[must_use]
pub fn pending_steps(room: &str, doc: &AutoCommit, authority: &DeviceCert) -> Vec<GovernorHandoff> {
    follow(room, authority, &logged(doc))
}

/// Whether `doc`'s handoff log moves the room's governance from the device
/// `from` on to the device `to`, through one handoff or several: `to` then
/// covers the room's records in `from`'s place, under its own sequence.
/// Read from the log alone: only the governor writes it, and a member
/// follows only handoffs that verify.
pub(crate) fn handed_between(doc: &AutoCommit, from: &[u8; 32], to: &[u8; 32]) -> bool {
    let mut governor = *from;
    for handoff in logged(doc) {
        if handoff.previous.device_public_key == governor {
            governor = handoff.successor().device_public_key;
            if governor == *to {
                return true;
            }
        }
    }
    false
}

/// The bytes of every handoff statement `doc` holds, for "unchanged"
/// comparisons: only the governor writes them.
pub(crate) fn governed_bytes(doc: Option<&AutoCommit>) -> Vec<(String, Vec<u8>)> {
    let Some(doc) = doc else {
        return Vec::new();
    };
    // Whatever a key holds, so a value of another shape than the governor
    // writes is a change too.
    let held = |value: Option<(Value<'_>, automerge::ObjId)>| match value {
        Some((Value::Scalar(value), _)) => Some(match value.as_ref() {
            ScalarValue::Bytes(bytes) => bytes.clone(),
            other => format!("{other:?}").into_bytes(),
        }),
        Some((Value::Object(kind), _)) => Some(format!("{kind:?}").into_bytes()),
        None => None,
    };
    let mut statements = Vec::new();
    for key in [OWNERSHIP_OFFER_KEY, GOVERNOR_HANDOFFS_KEY] {
        let value = doc.get(ROOT, key).ok().flatten();
        let map = match &value {
            Some((Value::Object(_), map)) => Some(map.clone()),
            _ => None,
        };
        if let Some(bytes) = held(value) {
            statements.push((key.to_owned(), bytes));
        }
        if let Some(map) = map {
            for entry in doc.keys(&map) {
                if let Some(bytes) = held(doc.get(&map, &entry).ok().flatten()) {
                    statements.push((format!("{key}/{entry}"), bytes));
                }
            }
        }
    }
    statements
}

#[cfg(test)]
pub(crate) mod tests;
