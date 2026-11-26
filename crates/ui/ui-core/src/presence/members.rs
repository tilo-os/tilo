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

    /// Take who the workspace's members are, by id, without the rest of its member list: a
    /// surface that heard the list apart from its rows (a page, whose host names its people)
    /// still offers only them, and nobody a device of theirs happened to name.
    pub fn set_workspace_members(&mut self, members: impl IntoIterator<Item = MemberId>) {
        self.members = members.into_iter().collect();
    }

    /// Whether this surface has heard who the workspace's members are: a list of its people
    /// offers them ([`Self::is_member`]) then, and whatever else presence knows of nobody.
    #[must_use]
    pub fn knows_members(&self) -> bool {
        !self.members.is_empty()
    }

    /// Whether `id` is one of the members the workspace's last member list named.
    #[must_use]
    pub fn is_member(&self, id: &MemberId) -> bool {
        self.members.contains(id)
    }

    /// What this session's own member is shown as, as anyone else names them; `None` until the
    /// workspace has named them.
    pub fn self_label(&self) -> Option<String> {
        self.self_name()?;
        self.self_id().map(|id| self.display_name(id))
    }

    /// Take the workspace's members as its `workspace/members` resource answers them, a complete
    /// list each time: when each joined, what this session's own member and everyone else is
    /// called and the colour they wear. Whoever left the list since the last one, or is on it as
    /// removed, is no longer here; a removed member's name is the records' to read
    /// ([`tilo_resource::members`]).
    pub fn apply_members(&mut self, members: Vec<Member>) {
        let members: Vec<Member> = members
            .into_iter()
            .filter(|member| !member.removed)
            .collect();
        let current: std::collections::HashSet<MemberId> =
            members.iter().map(|member| member.id.clone()).collect();
        let departed: Vec<MemberId> = self.members.difference(&current).cloned().collect();
        for departed in &departed {
            self.unregister_peer(departed);
        }
        for member in &members {
            self.set_joined_at(member.id.clone(), member.joined_at);
            // The picture they set in their profile, as the list names it: a face reads it
            // wherever a view shows them as an author, and every chip of them draws it.
            match &member.avatar {
                Some(picture) => {
                    self.pictures.insert(member.id.clone(), picture.clone());
                }
                None => {
                    self.pictures.remove(&member.id);
                }
            }
            crate::people_photos::set_encoded(
                member.id.id(),
                member.avatar.as_ref().map(|picture| picture.0.as_slice()),
            );
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
    }

    /// The person `user` acts for, if it is an agent or a bot acting for one.
    pub fn acting_for(&self, user: &MemberId) -> Option<&MemberId> {
        self.acting_for.get(user)
    }
}
