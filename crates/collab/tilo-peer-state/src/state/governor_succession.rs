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
    /// The state a successor governs from: the first of `sources` that holds
    /// the previous governor's history up to the succession's root-signed
    /// cutoff, and nothing past it that changes governance
    /// ([`GovernorSuccession::check_history`]). A relay's retention is
    /// replayed into a replica pinned to the previous governor, so that every
    /// message passes the checks any member applies; a replica that follows
    /// exactly this succession from its records is pinned back first.
    ///
    /// # Errors
    ///
    /// Returns why no source holds the previous governor's history exactly
    /// up to the cutoff. An invalid succession or a known revoked successor
    /// refuses custody even when another history source would verify.
    pub fn take_custody(
        succession: &GovernorSuccession,
        sources: impl IntoIterator<Item = Custody>,
        now: SystemTime,
    ) -> Result<Self, String> {
        succession.verify(&succession.room, &succession.previous, now_ns(now))?;
        let mut refusals = Vec::new();
        let mut custody = None;
        for source in sources {
            let candidate = match source {
                Custody::Replica(peer) => Ok(Self::unfollow(*peer, succession)),
                Custody::Retained(messages) => Self::replay(succession, messages, now),
            };
            if candidate
                .as_ref()
                .is_ok_and(|peer| peer.core.revocations.revokes(&succession.successor))
            {
                // A verified revocation is permanent evidence, not a bad
                // history source that a stale fallback may replace.
                return Err("the successor is revoked".into());
            }
            let checked = candidate.and_then(|peer| {
                if peer.core.room != succession.room || peer.core.authority != succession.previous {
                    return Err("custody is of another room or governor".into());
                }
                succession.check_cutoff(
                    peer.core
                        .room_doc
                        .as_deref()
                        .ok_or("custody holds no room governance")?,
                )?;
                Ok(peer)
            });
            match checked {
                // Preserve source preference while still observing verified
                // revocations carried by every remaining custody source.
                Ok(peer) if custody.is_none() => custody = Some(peer),
                Ok(_) => {}
                Err(error) => refusals.push(error),
            }
        }
        custody.ok_or_else(|| {
            format!(
                "no custody of the room verifies against its succession: {}",
                refusals.join("; ")
            )
        })
    }

    /// A replica that already follows exactly `succession` from its records
    /// alone, pinned back to the previous governor so the successor takes it
    /// over as any custody: its history does not record the succession yet.
    fn unfollow(mut peer: Self, succession: &GovernorSuccession) -> Self {
        if peer.core.authority == succession.successor && peer.core.followed == [succession.clone()]
        {
            peer.core.authority = succession.previous.clone();
            peer.core.followed.clear();
        }
        peer
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
            // Relayed successions would move the replay past the governor
            // whose history it rebuilds.
            if matches!(message, NeighbourMsg::GovernorSuccession { .. }) {
                continue;
            }
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
        let chain = self.succession_chain(candidate, source, now)?;
        if let Some(chain) = &chain {
            succession::check_chain(chain, candidate, false)?;
        }
        Ok(chain)
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
        let admitted = self.core.room_doc.as_deref();
        let before = match admitted {
            Some(admitted) => succession::recorded_all(admitted)?,
            None => BTreeMap::new(),
        };
        let after = succession::recorded_all(candidate)?;
        if before == after {
            return Ok(());
        }
        // Successions followed from their records alone count as accepted.
        let known = succession::merged(admitted, &self.core.followed)?;
        let root = self.core.authority.user_public_key;
        for (key, records) in &after {
            let Some(accepted) = known.get(key) else {
                continue;
            };
            let rival = records.iter().find(|record| {
                !accepted
                    .iter()
                    .any(|known| known.successor == record.successor)
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
        if (self.core.adopting && only_added) || self.joins_successor(&after)? {
            return Ok(());
        }
        Err("only a successor taking the room over records a succession".into())
    }

    /// Whether this replica, pinned to the governor it was given and holding
    /// no room history yet, takes a room whose successions `recorded` are
    /// exactly the chain that leads to that governor: a device invited by a
    /// successor joins its room as it stands.
    fn joins_successor(&mut self, recorded: &succession::Recorded) -> Result<bool, AdmissionError> {
        if self.core.room_doc.is_some() {
            return Ok(false);
        }
        let lineage = match succession::lineage(recorded, &self.core.room, &self.core.authority) {
            Ok(lineage) => lineage,
            Err(error) if error.starts_with(CONTESTED) => {
                self.core.contested = Some(error.clone());
                return Err(error.into());
            }
            Err(error) => return Err(error.into()),
        };
        let mut chained = BTreeMap::<String, Vec<GovernorSuccession>>::new();
        for link in lineage {
            chained.entry(link.key()).or_default().push(link);
        }
        Ok(!chained.is_empty() && chained == *recorded)
    }

    /// Follow the root-signed succession `records` any device relayed, before
    /// the room history that carries them reaches this replica: from the
    /// governor it follows, each record verified against that governor's
    /// root, the root's revocation of it is held and its successor followed.
    /// From then on the revoked governor is refused, and the successor's room
    /// is taken once it arrives ([`Self::adopt_followed`]). A record naming a
    /// different successor for a governor already followed contests the room.
    pub(super) fn follow_relayed(
        &mut self,
        records: Vec<GovernorSuccession>,
        now: SystemTime,
    ) -> Result<AdmissionReport, AdmissionError> {
        if let Some(reason) = &self.core.contested {
            return Err(reason.clone().into());
        }
        let admitted = self.core.room_doc.as_deref();
        let known = succession::merged(admitted, &self.core.followed)?;
        let mut offered = self.core.followed.clone();
        offered.extend(records);
        let all = succession::merged(admitted, &offered)?;
        let root = self.core.authority.user_public_key;
        for (key, accepted) in &known {
            let rival = all.get(key).into_iter().flatten().find(|record| {
                !accepted
                    .iter()
                    .any(|known| known.successor == record.successor)
                    && record.previous.user_public_key == root
                    && record
                        .verify(&self.core.room, &record.previous, now_ns(now))
                        .is_ok()
            });
            if let Some(rival) = rival {
                let reason = format!(
                    "{CONTESTED}: the identity's root named both {} and {} to succeed governor {key}; neither is followed",
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
        let chain = match succession::chain_in(
            &all,
            admitted,
            &self.core.room,
            &self.core.authority,
            succession::Walk::Relayed {
                now_ns: now_ns(now),
            },
        ) {
            Ok(chain) => chain,
            Err(error) if error.starts_with(CONTESTED) => {
                self.core.contested = Some(error.clone());
                return Err(error.into());
            }
            Err(error) => return Err(error.into()),
        };
        if chain.is_empty() {
            return Ok(AdmissionReport::metadata(super::ReceiveOutcome::default()));
        }
        if let Some(admitted) = admitted {
            succession::check_chain(&chain, admitted, true)?;
        }
        let mut staged = self.clone();
        if staged.core.followed.len().saturating_add(chain.len()) > succession::MAX_SUCCESSIONS {
            return Err("governor succession chain is too long".into());
        }
        staged.follow_links(chain.clone(), now)?;
        staged.core.followed.extend(chain);
        *self = staged;
        Ok(AdmissionReport::metadata(super::ReceiveOutcome {
            metadata_changed: true,
            ..Default::default()
        }))
    }

    /// Re-pin to the end of `chain`: each link's revocation held, its
    /// successor's certificate admitted.
    fn follow_links(
        &mut self,
        chain: Vec<GovernorSuccession>,
        now: SystemTime,
    ) -> Result<(), AdmissionError> {
        for link in chain {
            if self
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
            self.core
                .revocations
                .insert(link.revocation.clone())
                .map_err(err)?;
            self.core.certificates.insert(
                link.successor.device_public_key,
                VerifiedCertificate::new(link.successor.clone(), now_ns(now))?,
            );
            self.core.authority = link.successor;
        }
        Ok(())
    }

    /// When this replica follows successions its room history does not
    /// record yet and `candidate`, sent by the governor it follows, records
    /// them all: the chain to admit `candidate` with, as when it came from the
    /// successor directly. Each cutoff is checked against `candidate`.
    pub(super) fn followed_chain(
        &mut self,
        candidate: &AutoCommit,
        now: SystemTime,
    ) -> Result<Option<Vec<GovernorSuccession>>, AdmissionError> {
        if self.core.followed.is_empty() {
            return Ok(None);
        }
        self.check_contest(candidate, now)?;
        let recorded = succession::recorded_all(candidate)?;
        let carried = self.core.followed.iter().all(|link| {
            recorded
                .get(&link.key())
                .is_some_and(|records| records.contains(link))
        });
        if !carried {
            return Ok(None);
        }
        succession::check_chain(&self.core.followed, candidate, false)?;
        Ok(Some(self.core.followed.clone()))
    }

    /// The root-signed successions this replica knows of, recorded or
    /// followed, as any device relays them: a member that cannot reach the
    /// successor learns from this who governs. `None` when there are none.
    ///
    /// # Errors
    ///
    /// Returns an error if a recorded succession does not decode or encode.
    pub fn succession_message(&self) -> Result<Option<NeighbourMsg>, String> {
        let all = succession::merged(self.core.room_doc.as_deref(), &self.core.followed)?;
        if all.is_empty() {
            return Ok(None);
        }
        Ok(Some(NeighbourMsg::GovernorSuccession {
            room: self.core.room.clone(),
            records: succession::encode_relayed(&all)?,
        }))
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
        staged.follow_links(chain, now)?;
        staged.core.adopting = true;
        let report =
            staged.receive_with_context(source, message, now, retained_replay, strict_report);
        staged.core.adopting = false;
        let report = report?;
        // The change records exactly the successions it is followed for.
        let recorded = succession::recorded_all(
            staged
                .core
                .room_doc
                .as_deref()
                .ok_or("room governance missing")?,
        )?;
        if recorded != expected {
            return Err("a succession change records other successions".into());
        }
        staged.core.followed.retain(|link| {
            !recorded
                .get(&link.key())
                .is_some_and(|records| records.contains(link))
        });
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
        for records in recorded
            .values()
            .chain(succession::merged(None, &self.core.followed)?.values())
        {
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
                    .is_some_and(|record| record.check_recorded_history(candidate).is_ok());
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
        succession::merged(self.core.room_doc.as_deref(), &self.core.followed)
            .is_ok_and(|all| succession::succeeded_in(&all, author, &current))
    }
}

#[cfg(test)]
mod regression_tests;
