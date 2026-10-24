//! A room's ownership moving to another person ([`crate::handoff`]): the
//! governor records the owner's offer, takes it back, or commits its
//! handoff; every member follows a committed handoff to the new governor.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{SigningKey, StatePeer, SystemTime, VerifiedCertificate, err};
use crate::handoff::{
    self, GOVERNOR_HANDOFFS_KEY, GovernorHandoff, HandoffError, OWNERSHIP_OFFER_KEY, OwnershipOffer,
};
use automerge::{ObjType, ROOT, ReadDoc as _, ScalarValue, transaction::Transactable as _};
use tilo_action_policy::Role;
use tilo_desktop_model::projection;

/// Why the governor refused to write an ownership statement.
#[derive(Debug)]
pub enum OwnershipWriteError {
    /// The statement itself is refused.
    Handoff(HandoffError),
    /// The writer does not govern the room, or the document did not take it.
    Governance(String),
}

impl std::fmt::Display for OwnershipWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Handoff(error) => write!(f, "{error}"),
            Self::Governance(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for OwnershipWriteError {}

impl From<HandoffError> for OwnershipWriteError {
    fn from(error: HandoffError) -> Self {
        Self::Handoff(error)
    }
}

impl PeerGovernance<'_> {
    /// The owner's open offer of the room to another person, if any.
    #[must_use]
    pub fn ownership_offer(self) -> Option<OwnershipOffer> {
        handoff::open_offer(self.peer.core.room_doc.as_deref()?)
    }

    /// Every handoff of the room's governance its governance logs, in
    /// generation order.
    #[must_use]
    pub fn governor_handoffs(self) -> Vec<GovernorHandoff> {
        self.peer
            .core
            .room_doc
            .as_deref()
            .map(handoff::logged)
            .unwrap_or_default()
    }

    /// The handoff that moved the room's governance on from the device this
    /// peer is pinned to, when its governance logs one: the room is no
    /// longer governed by it. The last one, when several follow each other.
    #[must_use]
    pub fn handed_on(self) -> Option<GovernorHandoff> {
        let doc = self.peer.core.room_doc.as_deref()?;
        handoff::pending_steps(&self.peer.core.room, doc, &self.peer.core.authority).pop()
    }
}

impl PeerGovernanceMut<'_> {
    /// Record `offer`, the owner's offer of the room to another person, as
    /// the room's open offer in place of any before it, as the governor
    /// `key`.
    ///
    /// # Errors
    ///
    /// The offer is not the owner's, or `key` does not govern the room.
    pub fn offer_ownership(
        self,
        offer: &OwnershipOffer,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, OwnershipWriteError> {
        if self.peer.governance().handed_on().is_some() {
            return Err(HandoffError::Moved.into());
        }
        handoff::check_offer(&self.peer.core.room, &self.peer.core.authority, offer)?;
        let bytes = postcard::to_stdvec(offer)
            .map_err(|error| HandoffError::Encoding(error.to_string()))?;
        self.author(key, now, |doc| {
            doc.put(ROOT, OWNERSHIP_OFFER_KEY, ScalarValue::Bytes(bytes))
        })
        .map_err(OwnershipWriteError::Governance)
    }

    /// Take the open offer `offer_id` back, as the governor `key`. Taking
    /// back an offer that is no longer open changes nothing.
    ///
    /// # Errors
    ///
    /// `key` does not govern the room.
    pub fn withdraw_ownership_offer(
        self,
        offer_id: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, OwnershipWriteError> {
        if self
            .peer
            .governance()
            .ownership_offer()
            .is_none_or(|offer| offer.offer_id != offer_id)
        {
            return Ok(false);
        }
        self.author(key, now, |doc| doc.delete(ROOT, OWNERSHIP_OFFER_KEY))
            .map_err(OwnershipWriteError::Governance)
    }

    /// Commit `handoff` as the governor `key`: log it, close the offer it
    /// accepts, and make the person it hands the room to its owner. The
    /// previous owner stays in the room as an administrator. From then on
    /// this peer's governor governs the room no longer.
    ///
    /// # Errors
    ///
    /// The handoff does not verify against the room's open offer and its
    /// governor, was committed before, or `key` does not govern the room.
    pub fn commit_handoff(
        self,
        handoff: &GovernorHandoff,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, OwnershipWriteError> {
        let governance = self.peer.governance();
        if governance.handed_on().is_some() {
            return Err(HandoffError::Moved.into());
        }
        let logged = governance.governor_handoffs();
        let offer_id = hex::encode(handoff.offer.offer_id);
        if logged
            .iter()
            .any(|entry| entry.offer.offer_id == handoff.offer.offer_id)
        {
            return Err(HandoffError::Replayed.into());
        }
        if governance.ownership_offer().as_ref() != Some(&handoff.offer) {
            return Err(HandoffError::WrongOffer.into());
        }
        let after = logged
            .iter()
            .map(|entry| entry.generation)
            .max()
            .unwrap_or(0);
        handoff.verify(&self.peer.core.room, &self.peer.core.authority, after)?;
        let bytes = postcard::to_stdvec(handoff)
            .map_err(|error| HandoffError::Encoding(error.to_string()))?;
        let room = self.peer.core.room.clone();
        let previous = hex::encode(handoff.offer.owner);
        let next = hex::encode(handoff.offer.recipient);
        self.author(key, now, |doc| {
            let log = match doc.get(ROOT, GOVERNOR_HANDOFFS_KEY)? {
                Some((automerge::Value::Object(ObjType::Map), log)) => log,
                _ => doc.put_object(ROOT, GOVERNOR_HANDOFFS_KEY, ObjType::Map)?,
            };
            doc.put(&log, offer_id.as_str(), ScalarValue::Bytes(bytes))?;
            doc.delete(ROOT, OWNERSHIP_OFFER_KEY)?;
            let mut policy = projection::room_policy_of(doc, &room);
            policy.owner = Some(next.clone());
            policy.members.remove(&next);
            policy.members.insert(previous.clone(), Role::Owner);
            projection::project_room_policy(doc, &policy)
        })
        .map_err(OwnershipWriteError::Governance)
    }
}

impl StatePeer {
    /// Follow every handoff the room's governance logs from the device this
    /// peer is pinned to ([`handoff::follow`]): the device each hands the
    /// room to governs it from then on. Returns the handoffs followed, in
    /// order. No clock is read: a handoff its governor committed stays
    /// valid, whatever this device's clock says.
    ///
    /// # Errors
    ///
    /// A successor is revoked or its certificate does not verify; nothing
    /// changes then.
    pub fn follow_handoffs(&mut self) -> Result<Vec<GovernorHandoff>, String> {
        let Some(doc) = self.core.room_doc.as_deref() else {
            return Ok(Vec::new());
        };
        let steps = handoff::pending_steps(&self.core.room, doc, &self.core.authority);
        if steps.is_empty() {
            return Ok(steps);
        }
        let mut candidate = self.clone();
        for step in &steps {
            let successor = step.successor().clone();
            if candidate
                .core
                .revocations
                .is_revoked(&successor.device_public_key)
            {
                return Err("the room was handed to a revoked device".into());
            }
            candidate.core.certificates.insert(
                successor.device_public_key,
                VerifiedCertificate::historical_typed(successor.clone()).map_err(err)?,
            );
            candidate.core.authority = successor;
        }
        *self = candidate;
        Ok(steps)
    }
}

#[cfg(test)]
mod tests;
