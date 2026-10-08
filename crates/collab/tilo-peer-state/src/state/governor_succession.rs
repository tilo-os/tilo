//! A replica's side of governor succession ([`crate::succession`]): taking
//! the room over as the successor, and following a successor as a member.
use super::{
    AdmissionError, AdmissionReport, AutoCommit, GovernedIdentity, NeighbourMsg, ObjType, ROOT,
    ReadDoc, SigningKey, StatePeer, SystemTime, Transactable, err, now_ns, room,
};
use crate::succession::{self, GovernorSuccession};
use crate::verified_certificate::VerifiedCertificate;
use std::collections::BTreeMap;

/// How every refusal that contests a room begins.
const CONTESTED: &str = "contested governor succession";

/// Where a successor takes the room's state from.
pub enum Custody {
    /// A replica of the room kept by a device of the governing identity.
    Replica(Box<StatePeer>),
    /// What a relay retains of the room: each message with the device that
    /// published it, as the relay authenticated it.
    Retained(Vec<([u8; 32], NeighbourMsg)>),
}

impl StatePeer {
    /// The state a successor governs from: the first of `sources` whose room
    /// history the succession's root-signed cutoff vouches for. A replica of
    /// the identity's own devices comes first; a relay's retention is the
    /// fallback, replayed into a replica pinned to the previous governor so
    /// that every message passes the checks any member applies.
    ///
    /// # Errors
    ///
    /// Returns why no source holds the previous governor's history exactly
    /// up to the cutoff.
    pub fn take_custody(
        succession: &GovernorSuccession,
        sources: impl IntoIterator<Item = Custody>,
        now: SystemTime,
    ) -> Result<Self, String> {
        let mut refusals = Vec::new();
        for source in sources {
            let candidate = match source {
                Custody::Replica(peer) => Ok(*peer),
                Custody::Retained(messages) => Self::replay(succession, messages, now),
            };
            let checked = candidate.and_then(|peer| {
                if peer.core.room != succession.room || peer.core.authority != succession.previous {
                    return Err("custody is of another room or governor".into());
                }
                succession.check_cutoff(
                    &**peer
                        .core
                        .room_doc
                        .as_ref()
                        .ok_or("custody holds no room governance")?,
                )?;
                Ok(peer)
            });
            match checked {
                Ok(peer) => return Ok(peer),
                Err(error) => refusals.push(error),
            }
        }
        Err(format!(
            "no custody of the room verifies against its succession: {}",
            refusals.join("; ")
        ))
    }

    fn replay(
        succession: &GovernorSuccession,
        messages: Vec<([u8; 32], NeighbourMsg)>,
        now: SystemTime,
    ) -> Result<Self, String> {
        let mut peer = Self::new(
            &succession.room,
            succession.previous.clone(),
            vec![succession.previous.clone()],
            tilo_user_identity::RevocationList::new(),
            now,
        )?;
        peer.select_admission_v5().map_err(err)?;
        for (author, message) in messages {
            // A member's write that no longer admits is not the room's
            // governance; the cutoff check decides whether what remains is.
            let _ = peer.receive_from(
                super::TransportOrigin::RetainedRelayDelta(author),
                message,
                now,
            );
        }
        Ok(peer)
    }

    /// Take the room over as `succession`'s successor, signing with `key`:
    /// record the succession in the room document with the successor's own
    /// governed row and the root's revocation of the previous governor, and
    /// re-pin this replica to the successor exactly as a member does when
    /// that change reaches it. The caller then publishes the room
    /// ([`Self::room_message`]).
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the successor's, the succession does
    /// not hand this replica's governor to it, or this replica's room history
    /// is not the previous governor's up to the cutoff.
    pub fn assume_governance(
        &mut self,
        succession: &GovernorSuccession,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        if key.verifying_key().to_bytes() != succession.successor.device_public_key {
            return Err("only the successor takes the room over".into());
        }
        succession.verify(&self.core.room, &self.core.authority, now_ns(now))?;
        let admitted = self
            .core
            .room_doc
            .as_deref()
            .ok_or("room governance missing")?;
        succession.check_cutoff(admitted)?;
        let mut doc: AutoCommit = admitted.clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            succession.successor.device_public_key.to_vec(),
        ));
        let successions = match doc.get(ROOT, succession::SUCCESSIONS_FIELD).map_err(err)? {
            Some((_, map)) => map,
            None => doc
                .put_object(ROOT, succession::SUCCESSIONS_FIELD, ObjType::Map)
                .map_err(err)?,
        };
        doc.put(
            &successions,
            succession.key(),
            automerge::ScalarValue::Bytes(postcard::to_stdvec(succession).map_err(err)?),
        )
        .map_err(err)?;
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("governed identities missing")?
            .1;
        let names = doc
            .get(ROOT, "peers")
            .map_err(err)?
            .ok_or("governed peers missing")?
            .1;
        let rows = room::governance_of(Some(&doc)).3;
        let previous = hex::encode(succession.previous.device_public_key);
        if let Some(bytes) = rows.get(&previous) {
            let mut row: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            if !row.revocations.contains(&succession.revocation) {
                row.revocations.push(succession.revocation.clone());
                row.revocations
                    .sort_by_key(|entry| entry.revoked_device_public_key);
                doc.put(
                    &identities,
                    previous,
                    automerge::ScalarValue::Bytes(serde_json::to_vec(&row).map_err(err)?),
                )
                .map_err(err)?;
            }
        }
        let successor = hex::encode(succession.successor.device_public_key);
        if !rows.contains_key(&successor) {
            let row = GovernedIdentity {
                device_cert: succession.successor.clone(),
                revocations: Vec::new(),
                admission: None,
                admissions: Vec::new(),
                workspace_delegations: Vec::new(),
            };
            doc.put(
                &identities,
                &successor,
                automerge::ScalarValue::Bytes(serde_json::to_vec(&row).map_err(err)?),
            )
            .map_err(err)?;
            doc.put(
                &names,
                &successor,
                succession.successor.device_label.as_str(),
            )
            .map_err(err)?;
        }
        doc.commit();
        room::media::refresh_authorization(admitted, &mut doc).map_err(err)?;
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let room = self.core.room.clone();
        self.receive_from(
            super::TransportOrigin::RelayDelta(succession.successor.device_public_key),
            NeighbourMsg::SyncResp { changes, room },
            now,
        )
        .map(|_| ())
    }

    /// A refused message that contested the room leaves it contested: the
    /// refusal discards everything else the message would have changed.
    pub(super) fn keep_contest(&mut self, refused: &Self) {
        if self.core.contested.is_none() {
            self.core.contested.clone_from(&refused.core.contested);
        }
    }

    /// As [`Self::keep_contest`], from a refusal whose candidate is gone.
    pub(super) fn keep_contest_from(&mut self, refusal: &AdmissionError) {
        let reason = refusal.to_string();
        if self.core.contested.is_none() && reason.starts_with(CONTESTED) {
            self.core.contested = Some(reason);
        }
    }

    /// Why this replica refuses the room's governance, when two different
    /// successors were recorded for one of its governors. It never picks one.
    #[must_use]
    pub fn contested_succession(&self) -> Option<&str> {
        self.core.contested.as_deref()
    }

    /// The governor this replica follows after `candidate`, when `candidate`
    /// records a succession from the current one and `source` is the
    /// successor it leads to. Refuses, and marks the room contested, when a
    /// governor has more than one successor or a recorded succession changes.
    pub(super) fn succession_to(
        &mut self,
        candidate: &AutoCommit,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<Option<Vec<GovernorSuccession>>, AdmissionError> {
        self.check_contest(candidate, now)?;
        self.succession_chain(candidate, source, now)
    }

    /// Refuse `candidate` when this replica's room is contested, or when it
    /// changes the successions the room records other than by adding the one
    /// the succession being adopted carries. When the identity's root signed
    /// a different successor for a governor whose succession the room
    /// already records, the room is contested: both are refused from then
    /// on, and [`Self::contested_succession`] says why.
    pub(super) fn guard_successions(
        &mut self,
        candidate: &AutoCommit,
        now: SystemTime,
    ) -> Result<(), AdmissionError> {
        if let Some(reason) = &self.core.contested {
            return Err(reason.clone().into());
        }
        let before = match self.core.room_doc.as_deref() {
            Some(admitted) => succession::recorded_all(admitted)?,
            None => BTreeMap::new(),
        };
        let after = succession::recorded_all(candidate)?;
        if before == after {
            return Ok(());
        }
        let root = self.core.authority.user_public_key;
        for (key, records) in &after {
            let Some(accepted) = before.get(key) else {
                continue;
            };
            let rival = records.iter().find(|record| {
                !accepted.contains(record)
                    && record.previous.user_public_key == root
                    && record
                        .verify(&self.core.room, &record.previous, now_ns(now))
                        .is_ok()
            });
            if let Some(rival) = rival {
                let reason = format!(
                    "contested governor succession: the identity's root named both {} and {} to succeed governor {key}; neither is followed",
                    accepted
                        .first()
                        .map_or_else(String::new, |record| hex::encode(
                            record.successor.device_public_key
                        )),
                    hex::encode(rival.successor.device_public_key),
                );
                self.core.contested = Some(reason.clone());
                return Err(reason.into());
            }
        }
        let only_added = before
            .iter()
            .all(|(key, records)| after.get(key) == Some(records));
        if !(self.core.adopting && only_added) {
            return Err("only a successor taking the room over records a succession".into());
        }
        Ok(())
    }

    /// Refuse when the room is contested, or `candidate` contests it.
    fn check_contest(
        &mut self,
        candidate: &AutoCommit,
        now: SystemTime,
    ) -> Result<(), AdmissionError> {
        match self.guard_successions(candidate, now) {
            Err(error) if self.core.contested.is_some() => Err(error),
            _ => Ok(()),
        }
    }

    pub(super) fn succession_chain(
        &mut self,
        candidate: &AutoCommit,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<Option<Vec<GovernorSuccession>>, AdmissionError> {
        let chain = match succession::chain(
            candidate,
            &self.core.room,
            &self.core.authority,
            succession::Walk::Follow {
                now_ns: now_ns(now),
            },
        ) {
            Ok(chain) => chain,
            Err(error) if error.starts_with("contested") => {
                self.core.contested = Some(error.clone());
                return Err(error.into());
            }
            Err(error) => return Err(error.into()),
        };
        if chain.is_empty() {
            return Ok(None);
        }
        let last = chain.last().map(|link| link.successor.device_public_key);
        if last != Some(source) {
            return Err("only the current successor publishes the succession".into());
        }
        Ok(Some(chain))
    }

    /// Follow `chain` to its successor, then admit `message` from it as the
    /// change that carries the succession. Nothing changes if it is refused.
    pub(super) fn adopt_succession(
        &mut self,
        chain: Vec<GovernorSuccession>,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        retained_replay: bool,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        let mut expected = match self.core.room_doc.as_deref() {
            Some(admitted) => succession::recorded_all(admitted)?,
            None => BTreeMap::new(),
        };
        for link in &chain {
            expected.insert(link.key(), vec![link.clone()]);
        }
        let mut staged = self.clone();
        for link in chain {
            if staged
                .core
                .revocations
                .revocation_of(
                    &link.successor.user_public_key,
                    &link.successor.device_public_key,
                )
                .is_some()
            {
                return Err("the successor is revoked".into());
            }
            staged
                .core
                .revocations
                .insert(link.revocation.clone())
                .map_err(err)?;
            staged.core.certificates.insert(
                link.successor.device_public_key,
                VerifiedCertificate::new(link.successor.clone(), now_ns(now))?,
            );
            staged.core.authority = link.successor;
        }
        staged.core.adopting = true;
        let report =
            staged.receive_with_context(source, message, now, retained_replay, strict_report);
        staged.core.adopting = false;
        let report = report?;
        // The change records exactly the successions it is followed for.
        if succession::recorded_all(
            staged
                .core
                .room_doc
                .as_deref()
                .ok_or("room governance missing")?,
        )? != expected
        {
            return Err("a succession change records other successions".into());
        }
        *self = staged;
        Ok(report)
    }
}

impl StatePeer {
    /// Refuse new room changes in `changes` written under a governing
    /// device's actor that `source` is not: a member cannot write as the
    /// governor, nor as a governor it succeeded. A governor republishing the
    /// history of a governor it succeeded carries that governor's changes
    /// only up to the cutoff the root vouched for.
    pub(super) fn check_governor_actors(
        &self,
        changes: &[Vec<u8>],
        candidate: &AutoCommit,
        source: [u8; 32],
    ) -> Result<(), AdmissionError> {
        let recorded = succession::recorded_all(candidate)?;
        let current = self.core.authority.device_public_key;
        let mut governors = std::collections::BTreeSet::from([current]);
        for records in recorded.values() {
            for record in records {
                governors.insert(record.previous.device_public_key);
                governors.insert(record.successor.device_public_key);
            }
        }
        let admitted = self.core.room_doc.as_deref();
        for bytes in changes {
            let change = automerge::Change::from_bytes(bytes.clone()).map_err(err)?;
            let actor: [u8; 32] = match change.actor_id().to_bytes().try_into() {
                Ok(actor) => actor,
                Err(_) => continue,
            };
            if actor == source || !governors.contains(&actor) {
                continue;
            }
            if admitted.is_some_and(|doc| doc.get_change_by_hash(&change.hash()).is_some()) {
                continue;
            }
            let predecessor = source == current
                && succession::succeeded(candidate, &actor, &current)
                && recorded
                    .get(&hex::encode(actor))
                    .and_then(|records| records.first())
                    .is_some_and(|record| record.check_cutoff(candidate).is_ok());
            if !predecessor {
                return Err("a room change is written as a governor by another device".into());
            }
        }
        Ok(())
    }

    /// Whether `author` signs governed receipts here: the current governor
    /// while it is not revoked, or a governor it succeeded, whose receipts
    /// written before its cutoff stay the room's.
    pub(super) fn governed_receipt_author(&self, author: &[u8; 32]) -> bool {
        let current = self.core.authority.device_public_key;
        if *author == current {
            return !self.core.revocations.revokes(&self.core.authority);
        }
        self.core
            .room_doc
            .as_deref()
            .is_some_and(|doc| succession::succeeded(doc, author, &current))
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use crate::{Custody, TransportOrigin};
    use tilo_user_identity::{DeviceCapabilities, RevocationEntry, RevocationReason};

    #[test]
    fn custody_refuses_governance_written_under_another_actor_after_cutoff() {
        let (mut peer, writer, _, member, now) = crate::admission_tests::seeded();
        let identity = SigningKey::from_bytes(&[1; 32]);
        let mut replacement = member;
        replacement.device_public_key =
            SigningKey::from_bytes(&[94; 32]).verifying_key().to_bytes();
        replacement.capabilities = DeviceCapabilities::primary();
        tilo_user_identity::sign_device_cert(&mut replacement, &identity).unwrap();
        let previous = peer.authority().clone();
        let mut revoked = RevocationEntry {
            user_public_key: previous.user_public_key,
            revoked_device_public_key: previous.device_public_key,
            revoked_at_ns: now_ns(now),
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revoked, &identity).unwrap();
        let mut document = peer.admitted_room().unwrap().clone();
        let heads = document.get_heads();
        let room = peer.room().to_owned();
        let record = GovernorSuccession::sign(
            &room,
            previous,
            revoked,
            replacement,
            heads.iter().map(|head| head.0).collect(),
            &identity,
        )
        .unwrap();
        record.verify(&room, peer.authority(), now_ns(now)).unwrap();
        StatePeer::take_custody(&record, [Custody::Replica(Box::new(peer.clone()))], now).unwrap();
        // The governor signs before peers hear its revocation, but writes
        // governance under a different Automerge actor after the root's cutoff.
        document.set_actor(automerge::ActorId::from(vec![95; 32]));
        document
            .put(ROOT, "owner", "untrusted-owner-after-cutoff")
            .unwrap();
        document.commit();
        let changes = document
            .get_changes(&heads)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        peer.receive_from(
            TransportOrigin::RelayDelta(writer.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp { changes, room },
            now,
        )
        .unwrap();
        assert!(
            StatePeer::take_custody(&record, [Custody::Replica(Box::new(peer))], now).is_err(),
            "custody must reject changed governance past the root-signed cutoff regardless of actor id"
        );
    }
}
