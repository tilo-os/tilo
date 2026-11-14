//! Who the workspace's members are, as its member list says: their names, whether each is a
//! person or software, whom software acts for, and which person a device is.

use collab_types::Member;
use tilo_sort::CompactSort as _;

use super::{ParticipantKind, PresenceCtx, person_key, stored};
use crate::permissions::UserId;

impl PresenceCtx {
    /// Everyone the workspace names, this session's own participant included,
    /// as they are shown, in an order every replica shares: who a people
    /// list or a mention picker offers.
    pub fn roster(&self) -> Vec<Member> {
        let mut ids: Vec<&UserId> = self.names.keys().collect();
        if let Some((own, _)) = &self.self_participant
            && !self.names.contains_key(own)
        {
            ids.push(own);
        }
        ids.compact_sort_by(|a, b| a.0.cmp(&b.0));
        ids.into_iter().map(|id| self.user(id)).collect()
    }

    /// What this session's own participant is shown as, numbered like
    /// anyone else's name; `None` until the workspace has named them.
    pub fn self_label(&self) -> Option<String> {
        self.self_name()?;
        self.self_id().map(|id| self.display_name(id))
    }

    /// Take the workspace's member list as the broker delivers it (`CollabEvent::Context`), a
    /// complete one each time: who is a person and who software, when each joined, what this
    /// session's own participant and everyone else is called and the colour they wear. Whoever
    /// left the list since the last one is no longer here.
    pub fn apply_members(&mut self, members: Vec<Member>) {
        let current: std::collections::HashSet<UserId> = members
            .iter()
            .map(|member| UserId(member.id.key()))
            .collect();
        let departed: Vec<UserId> = self.members.difference(&current).cloned().collect();
        for departed in departed {
            self.unregister_peer(&departed.0);
        }
        for member in &members {
            let user = UserId(member.id.key());
            self.set_participant_kind(&user, !member.id.is_person());
            self.set_joined_at(user, member.joined_at);
        }
        if let Some(me) = members.iter().find(|member| member.is_self) {
            let user = UserId(me.id.key());
            self.set_self_participant(user.clone(), me.name.clone());
            self.set_presence_hue(user, me.colour);
        } else {
            self.clear_self_participant();
        }
        // The others alone: the person's own row would put them in every list built from
        // presence.
        for member in members.into_iter().filter(|member| !member.is_self) {
            let user = UserId(member.id.key());
            // Relabel only: the list does not say where a peer's pointer is, so it must not
            // mint one at the origin.
            self.set_display_name(user.clone(), member.name);
            self.set_presence_hue(user.clone(), member.colour);
            self.set_participant_kind(
                user,
                if member.id.is_person() {
                    ParticipantKind::Person
                } else {
                    ParticipantKind::Agent
                },
            );
        }
        self.members = current;
    }

    /// Seed the authenticated participant kind announced by the workspace. Both
    /// the legacy boolean and the typed enum map to the same authenticated
    /// kind, so changing names never changes a participant's principal kind.
    pub fn set_participant_kind(
        &mut self,
        user: impl AsRef<UserId>,
        kind: impl Into<ParticipantKind>,
    ) {
        let user = person_key(user.as_ref().clone());
        let kind = kind.into();
        if self.participant_kinds.insert(user.clone(), kind) == Some(kind) {
            return;
        }
        self.refresh_labels();
        self.relabel_cursor(&user);
    }

    pub fn participant_kind(&self, user: &UserId) -> ParticipantKind {
        stored(&self.participant_kinds, user)
            .copied()
            .unwrap_or_default()
    }

    /// Whether `user` is software acting for someone (an agent, a Shortcut,
    /// an app) rather than a person: drawn with the agent mark.
    pub fn is_agent(&self, user: &UserId) -> bool {
        self.participant_kind(user) != ParticipantKind::Person
    }

    /// Record whom `user` acts for, as the broker admitted it; `None` when
    /// it acts for nobody in particular.
    pub fn set_acting_for(&mut self, user: impl AsRef<UserId>, person: Option<UserId>) {
        let user = user.as_ref().clone();
        match person {
            Some(person) => self.acting_for.insert(user.clone(), person_key(person)),
            None => self.acting_for.remove(&user),
        };
        let shown = self.display_name(&user);
        for cursor in &mut self.remote_cursors {
            if cursor.user_id == user.0 {
                cursor.label.clone_from(&shown);
            }
        }
    }

    /// The person `user` acts for, if it is an agent, a Shortcut or an app
    /// acting for one.
    pub fn acting_for(&self, user: &UserId) -> Option<&UserId> {
        stored(&self.acting_for, user)
    }

    /// Replace which person each participant here is a device of, as the
    /// workspace's host paired them: `(participant, person)`. Where presence
    /// already names people, there is nothing to pair.
    pub fn set_persons(&mut self, pairs: impl IntoIterator<Item = (UserId, UserId)>) {
        self.persons = pairs
            .into_iter()
            .filter(|(participant, person)| participant != person && !person.0.trim().is_empty())
            .collect();
    }

    /// The person participant `user` is a device of, when the workspace's host
    /// paired them; `None` where presence names the person itself.
    pub fn person_of(&self, user: &UserId) -> Option<&UserId> {
        self.persons.get(user)
    }
}
