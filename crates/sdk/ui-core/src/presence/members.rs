//! Who the workspace's members are, as its member list says: their names, whom software acts
//! for, and the colour each wears.

use collab_types::Member;
use tilo_sort::CompactSort as _;

use super::PresenceCtx;
use crate::permissions::MemberId;

impl PresenceCtx {
    /// Everyone the workspace names, this session's own member included,
    /// as they are shown, in an order every replica shares: who a people
    /// list or a mention picker offers.
    pub fn roster(&self) -> Vec<Member> {
        let mut ids: Vec<&MemberId> = self.names.keys().collect();
        if let Some((own, _)) = &self.self_member
            && !self.names.contains_key(own)
        {
            ids.push(own);
        }
        ids.compact_sort();
        ids.into_iter().map(|id| self.user(id)).collect()
    }

    /// What this session's own member is shown as, numbered like
    /// anyone else's name; `None` until the workspace has named them.
    pub fn self_label(&self) -> Option<String> {
        self.self_name()?;
        self.self_id().map(|id| self.display_name(id))
    }

    /// Take the workspace's member list as the broker delivers it (`CollabEvent::Context`), a
    /// complete one each time: when each joined, what this session's own member and
    /// everyone else is called and the colour they wear. Whoever left the list since the last
    /// one is no longer here.
    pub fn apply_members(&mut self, members: Vec<Member>) {
        let current: std::collections::HashSet<MemberId> =
            members.iter().map(|member| member.id.clone()).collect();
        let departed: Vec<MemberId> = self.members.difference(&current).cloned().collect();
        for departed in &departed {
            self.unregister_peer(departed);
        }
        for member in &members {
            self.set_joined_at(member.id.clone(), member.joined_at);
        }
        if let Some(me) = members.iter().find(|member| member.is_self) {
            self.set_self_member(me.id.clone(), me.name.clone());
            self.set_presence_hue(me.id.clone(), me.colour);
        } else {
            self.clear_self_member();
        }
        // The others alone: the person's own row would put them in every list built from
        // presence.
        for member in members.into_iter().filter(|member| !member.is_self) {
            // Relabel only: the list does not say where a peer's pointer is, so it must not
            // mint one at the origin.
            self.set_display_name(member.id.clone(), member.name);
            self.set_presence_hue(member.id, member.colour);
        }
        self.members = current;
    }

    /// Record whom `user` acts for, as the broker admitted it; `None` when
    /// it acts for nobody in particular.
    pub fn set_acting_for(&mut self, user: MemberId, person: Option<MemberId>) {
        match person {
            Some(person) => self.acting_for.insert(user.clone(), person),
            None => self.acting_for.remove(&user),
        };
        self.relabel_cursor(&user);
    }

    /// The person `user` acts for, if it is an agent or a bot acting for one.
    pub fn acting_for(&self, user: &MemberId) -> Option<&MemberId> {
        self.acting_for.get(user)
    }
}
