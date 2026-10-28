//! The room's layout scope, which its governor alone writes.
//!
//! In a personal room each member's strips and windows stay on their device;
//! the room shares its apps' records and its people
//! ([`tilo_desktop_model::projection::RoomLayout`]). Its governor records the
//! scope ([`tilo_desktop_model::projection::ROOM_LAYOUT_KEY`]) for every
//! member to read. The layout itself is the room's shared desktop record; a
//! room document still holds the windows devices from before put there, and
//! the governor of a personal room removes them.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{ROOT, SystemTime, err};
use automerge::transaction::Transactable as _;
use ed25519_dalek::SigningKey;
use tilo_desktop_model::projection::{
    ROOM_LAYOUT_KEY, RoomLayout, room_layout, room_layout_choice,
};
use tilo_desktop_model::shared_record;

impl PeerGovernance<'_> {
    /// The room's layout scope, as its governance document records it:
    /// shared unless its governor chose otherwise.
    #[must_use]
    pub fn room_layout(self) -> RoomLayout {
        self.peer
            .core
            .room_doc
            .as_deref()
            .map_or_else(RoomLayout::default, room_layout)
    }

    /// The layout scope the governor chose, `None` while it chose none.
    #[must_use]
    pub fn room_layout_choice(self) -> Option<RoomLayout> {
        self.peer
            .core
            .room_doc
            .as_deref()
            .and_then(room_layout_choice)
    }
}

impl PeerGovernanceMut<'_> {
    /// Set the room's layout scope to `layout`, authored by the governor
    /// `key`. Choosing what the room already chose changes nothing. Returns
    /// whether it changed the room.
    pub fn author_room_layout(
        self,
        layout: RoomLayout,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        if self.peer.governance().room_layout_choice() == Some(layout) {
            return Ok(false);
        }
        let mut doc = self.peer.core.room_doc.clone().ok_or("missing room")?;
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        doc.put(ROOT, ROOM_LAYOUT_KEY, layout.id()).map_err(err)?;
        doc.commit();
        self.peer.apply_governance_doc(doc, &before, key, now)
    }

    /// Remove every strip and window someone made from the room's layout,
    /// with the groups left empty: in a room whose layout is personal, which
    /// apps a person has open and their titles stay on their devices, the
    /// governor's own included (its device takes them out itself). What the
    /// room itself holds (nobody made) stays. Returns whether it changed the
    /// room.
    pub fn drop_member_windows(self, key: &SigningKey, now: SystemTime) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        let mut doc = self.peer.core.room_doc.clone().ok_or("missing room")?;
        let before = doc.get_heads();
        let authors = shared_record::read(&*doc, None)
            .map(|desktop| desktop.authors)
            .unwrap_or_default();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let dropped = shared_record::device::withdraw(&mut doc, &|id| authors.contains_key(&id))
            .map_err(err)?;
        if !dropped {
            return Ok(false);
        }
        doc.commit();
        self.peer.apply_governance_doc(doc, &before, key, now)
    }
}

#[cfg(test)]
mod tests {
    use tilo_desktop_model::ids::compose_id;
    use tilo_desktop_model::projection::RoomLayout;
    use tilo_desktop_model::shared_record::{self, device};
    use tilo_desktop_model::width::WindowWidth;
    use tilo_desktop_model::{
        GroupId, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, Workspace, WorkspaceId,
    };
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const MEMBER: u16 = 42;

    fn window(salt: u16, counter: u64, title: &str) -> WindowGroup {
        let id: GroupId = compose_id(salt, counter);
        WindowGroup {
            id,
            tabs: vec![Tab {
                id,
                title: title.into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: title.into(),
                    note: String::new(),
                },
                app_title: None,
            }],
            active_tab: id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    fn changes_since(
        doc: &automerge::AutoCommit,
        before: &[automerge::ChangeHash],
    ) -> Vec<Vec<u8>> {
        doc.clone()
            .get_changes(before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect()
    }

    /// The member's strip holding one window titled `title`, as the changes
    /// the member's device writes into the room's layout, built on what
    /// `peer` admitted.
    fn member_windows(
        peer: &crate::StatePeer,
        member: &tilo_user_identity::DeviceCert,
        title: &str,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
        let mut doc = peer.admitted_room().ok_or("room missing")?.clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
        let room = RoomId::parse(peer.room()).ok_or("room id")?;
        let mut workspace = Workspace::in_room(WorkspaceId::of_room(room), room, "Room");
        let mut strip = Strip::new(StripId(compose_id(MEMBER, 1)), "Mine");
        strip.window_groups.push(window(MEMBER, 2, title));
        workspace.strips.push(strip);
        let person = hex::encode(member.user_public_key);
        let mine = |id: u64| tilo_desktop_model::peer_salt_of(id) == MEMBER;
        device::write(
            &mut doc,
            room,
            &workspace,
            &device::Writer {
                person: Some(&person),
                mine: &mine,
                retained: &|_| false,
                carried: tilo_desktop_model::shared_record::device::Writer::NOTHING_CARRIED,
            },
        )?;
        doc.commit();
        Ok(changes_since(&doc, &before))
    }

    fn windows(peer: &crate::StatePeer) -> usize {
        peer.admitted_room()
            .and_then(|doc| shared_record::read(doc, None))
            .map_or(0, |desktop| {
                desktop
                    .tree
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.strips)
                    .flat_map(|s| &s.window_groups)
                    .map(|g| g.tabs.len())
                    .sum()
            })
    }

    /// The governor makes a room personal: the windows a member's device
    /// put into the room document before go.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_personal_room_drops_its_members_windows() -> TestResult {
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert_eq!(peer.governance().room_layout(), RoomLayout::Shared);
        // While the room shares its layout, the member's window is admitted.
        let room = peer.room().to_owned();
        let changes = member_windows(&peer, &caller, "~/private")?;
        peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp { changes, room },
            now,
        )?;
        assert_eq!(windows(&peer), 1);

        assert!(
            peer.governance_mut()
                .author_room_layout(RoomLayout::Personal, &governor, now)?
        );
        assert!(
            !peer
                .governance_mut()
                .author_room_layout(RoomLayout::Personal, &governor, now)?,
            "choosing it again changes nothing"
        );
        assert_eq!(peer.governance().room_layout(), RoomLayout::Personal);
        assert!(peer.governance_mut().drop_member_windows(&governor, now)?);
        assert_eq!(windows(&peer), 0);
        assert!(!peer.governance_mut().drop_member_windows(&governor, now)?);
        Ok(())
    }

    /// In a personal room a member's device may still take its own windows
    /// out of the room, as it does once it reads the room is personal.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_member_takes_its_own_windows_out_of_a_personal_room() -> TestResult {
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        let room = peer.room().to_owned();
        let changes = member_windows(&peer, &caller, "Groceries")?;
        peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp {
                changes,
                room: room.clone(),
            },
            now,
        )?;
        assert!(
            peer.governance_mut()
                .author_room_layout(RoomLayout::Personal, &governor, now)?
        );
        let mut doc = peer.admitted_room().ok_or("room missing")?.clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(caller.device_public_key.to_vec()));
        assert!(device::withdraw(&mut doc, &|id| {
            tilo_desktop_model::ids::peer_salt_of(id) == MEMBER
        })?);
        doc.commit();
        let changes = changes_since(&doc, &before);
        peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp { changes, room },
            now,
        )?;
        assert_eq!(windows(&peer), 0);
        Ok(())
    }

    /// Any device of the owner sets the layout scope.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn another_device_of_the_owner_changes_the_rooms_layout_scope() -> TestResult {
        use automerge::transaction::Transactable as _;
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert!(
            peer.governance_mut()
                .author_room_layout(RoomLayout::Personal, &governor, now)?
        );
        let mut doc = peer.admitted_room().ok_or("room missing")?.clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(caller.device_public_key.to_vec()));
        doc.put(
            automerge::ROOT,
            tilo_desktop_model::projection::ROOM_LAYOUT_KEY,
            "shared",
        )?;
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let room = peer.room().to_owned();
        peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp { changes, room },
            now,
        )?;
        assert_eq!(peer.governance().room_layout(), RoomLayout::Shared);
        Ok(())
    }
}
