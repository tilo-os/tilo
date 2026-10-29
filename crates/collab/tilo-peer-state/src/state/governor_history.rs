//! A replica that wrote under its governor's key took a history of its
//! own: it takes the governor's back, and hands over what only it held.
use super::{StatePeer, SystemTime};
use crate::cached_document::CachedDocument;
use crate::room;

impl StatePeer {
    /// Whether this replica's room holds a change under the governor's key
    /// that `changes`, the governor's whole history of the room, numbers
    /// differently or lacks: written under that key by a replica other than
    /// the governor's, a history the governor's refuses from then on.
    #[must_use]
    pub fn diverges_from_governor(&self, changes: &[Vec<u8>]) -> bool {
        let Some(doc) = self.core.room_doc.as_deref() else {
            return false;
        };
        let actor = automerge::ActorId::from(self.core.authority.device_public_key.to_vec());
        let held = doc.committed_document();
        let numbered = held.applied_seq(&actor);
        let mut governed = 0;
        for change in changes
            .iter()
            .filter_map(|bytes| automerge::Change::try_from(bytes.as_slice()).ok())
            .filter(|change| change.actor_id() == &actor)
        {
            if change.seq() <= numbered && held.get_change_meta_by_hash(&change.hash()).is_none() {
                return true;
            }
            governed = governed.max(change.seq());
        }
        // Changes under the governor's key the governor never wrote: the
        // governor's next write numbers itself as the first of them.
        numbered > governed
    }

    /// Take `changes`, the governor's whole history of the room, in place
    /// of this replica's, when this replica's history diverged from it
    /// ([`Self::diverges_from_governor`]): the governor's is the room's.
    /// It is admitted as the governor's own, as any of its writes is.
    ///
    /// `None` when the replica was on the governor's history already. Else
    /// the membership changes this replica's own history made that the
    /// governor's lacks ([`Self::membership_beyond`]): grants given or
    /// taken back, roles set. They are no longer in this replica either,
    /// and are the caller's to ask of the governor again, as they were
    /// made; a member's change still in its device's custody is sent again
    /// on its own.
    ///
    /// # Errors
    /// The governor's history does not admit; this replica stays as it was.
    pub fn adopt_governor_history(
        &mut self,
        changes: &[Vec<u8>],
        now: SystemTime,
    ) -> Result<Option<Vec<tilo_action_policy::Change>>, String> {
        if !self.diverges_from_governor(changes) {
            return Ok(None);
        }
        let beyond = self.membership_beyond(changes)?;
        let governor = self.core.authority.device_public_key;
        let room = self.core.room.clone();
        let before = self.core.room_doc.take();
        match self.receive_room_changes(governor, &room, changes, now, false) {
            Ok(_) => Ok(Some(beyond)),
            Err(error) => {
                self.core.room_doc = before;
                Err(error.to_string())
            }
        }
    }

    /// The membership changes this replica's history made since it last
    /// shared the governor's, `changes`, that the governor's does not
    /// carry: a grant given that the governor lacks, one taken back that
    /// the governor still holds, a role set that the governor's leaves as
    /// it was. Each is a [`tilo_action_policy::Change`] as it was made, its
    /// grant naming whoever gave it.
    fn membership_beyond(
        &self,
        changes: &[Vec<u8>],
    ) -> Result<Vec<tilo_action_policy::Change>, String> {
        use tilo_action_policy::Change;
        let Some(replica) = self.core.room_doc.as_ref() else {
            return Ok(Vec::new());
        };
        let held = replica.committed_document();
        let shared: Vec<Vec<u8>> = changes
            .iter()
            .filter(|bytes| {
                automerge::Change::try_from(bytes.as_slice())
                    .is_ok_and(|change| held.get_change_meta_by_hash(&change.hash()).is_some())
            })
            .cloned()
            .collect();
        let ancestor = CachedDocument::from(room::candidate(None, &shared)?);
        let governor = CachedDocument::from(room::candidate(None, changes)?);
        let (ours, theirs, base) = (replica.policy(), governor.policy(), ancestor.policy());
        let find = |grants: &[tilo_action_policy::Grant], id: &tilo_action_policy::GrantId| {
            grants.iter().find(|grant| &grant.id == id).cloned()
        };
        let mut beyond = Vec::new();
        for grant in &ours.grants {
            if find(&base.grants, &grant.id).as_ref() != Some(grant)
                && find(&theirs.grants, &grant.id).as_ref() != Some(grant)
            {
                beyond.push(Change::Grant(grant.clone()));
            }
        }
        for grant in &base.grants {
            if find(&ours.grants, &grant.id).is_none() && find(&theirs.grants, &grant.id).is_some()
            {
                beyond.push(Change::Revoke(grant.id.clone()));
            }
        }
        for (person, role) in &ours.members {
            if base.members.get(person) != Some(role) && theirs.members.get(person) != Some(role) {
                beyond.push(Change::SetRole {
                    person: person.clone(),
                    role: *role,
                });
            }
        }
        for person in base.members.keys() {
            if !ours.members.contains_key(person) && theirs.members.contains_key(person) {
                beyond.push(Change::RemoveMember {
                    person: person.clone(),
                });
            }
        }
        Ok(beyond)
    }
}
