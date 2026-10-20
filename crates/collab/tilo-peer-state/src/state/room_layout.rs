//! The room's layout scope, which its governor alone writes, and what a
//! room whose layout is personal never holds: its members' windows.
//!
//! In a personal room each member's strips and windows stay on their device;
//! the room shares its apps' records and its people
//! ([`tilo_desktop_model::projection::RoomLayout`]). Its governor records the
//! scope ([`tilo_desktop_model::projection::ROOM_LAYOUT_KEY`]) for every
//! member to read, admits no member's change to the room's layout
//! ([`crate::room::admit::RoomRefusal::PersonalLayout`]), and removes the
//! windows it committed of them while the room still shared its layout.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{ROOT, SystemTime, err};
use automerge::transaction::Transactable as _;
use ed25519_dalek::SigningKey;
use tilo_desktop_model::ids::peer_salt_of;
use tilo_desktop_model::projection::{
    ROOM_LAYOUT_KEY, RoomLayout, contents_owners_of, drop_contents_except, room_layout,
    room_layout_choice,
};

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
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer or the change is
    /// refused.
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

    /// Remove every strip, window group and window a member published into
    /// the room's contents: each entry in a topology namespace registered to
    /// a device other than the governor `key`'s. What the governor or the
    /// room itself (an unregistered namespace) holds stays, and so does the
    /// namespace registry. Returns whether it changed the room.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer or the change is
    /// refused.
    pub fn drop_member_windows(self, key: &SigningKey, now: SystemTime) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        let mut doc = self.peer.core.room_doc.clone().ok_or("missing room")?;
        let before = doc.get_heads();
        let owners = contents_owners_of(&doc);
        let governor = hex::encode(key.verifying_key().to_bytes());
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let dropped = drop_contents_except(&mut doc, &|id| {
            owners
                .get(&peer_salt_of(id))
                .is_none_or(|owner| *owner == governor)
        });
        if dropped == 0 {
            return Ok(false);
        }
        doc.commit();
        self.peer.apply_governance_doc(doc, &before, key, now)
    }
}

#[cfg(test)]
mod tests {
    use automerge::ReadDoc as _;
    use tilo_desktop_model::ids::compose_id;
    use tilo_desktop_model::projection::{
        EmptyStrips, RoomLayout, contents_key, contents_of, project_owned_contents,
    };
    use tilo_desktop_model::width::WindowWidth;
    use tilo_desktop_model::{GroupId, Strip, StripId, Tab, TileContent, WindowGroup};
    type TestResult = Result<(), Box<dyn std::error::Error>>;

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
            }],
            active_tab: id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// `salt`'s strip holding one window titled `title`, as the room-document
    /// changes a member's device publishes, built on what `peer` admitted.
    fn member_windows(
        peer: &crate::StatePeer,
        salt: u16,
        title: &str,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
        let mut doc = peer.admitted_room().ok_or("room missing")?.clone();
        let before = doc.get_heads();
        let mut strip = Strip::new(StripId(compose_id(salt, 1)), "Mine");
        strip.window_groups.push(window(salt, 2, title));
        project_owned_contents(&mut doc, &[strip], salt, None, EmptyStrips::Skip)?;
        doc.commit();
        Ok(doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect())
    }

    // USER REQUIREMENT (2026-10-20): "windows should stay private by default"
    /// The governor makes a room personal: the windows it committed of a
    /// member before go (its own namespace registry stays), and from then
    /// on the room admits no member's window.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_personal_room_drops_and_refuses_its_members_windows() -> TestResult {
        const MEMBER: u16 = 42;
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert_eq!(peer.governance().room_layout(), RoomLayout::Shared);
        assert!(
            peer.governance_mut()
                .claim_topology_namespace(&caller, MEMBER, &governor, now)?
        );
        // While the room shares its layout, the member's window is admitted.
        let room = peer.room().to_owned();
        let changes = member_windows(&peer, MEMBER, "~/private")?;
        peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp {
                changes,
                room: room.clone(),
            },
            now,
        )?;
        assert_eq!(
            contents_of(peer.admitted_room().ok_or("room missing")?).len(),
            1
        );

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
        let room_doc = peer.admitted_room().ok_or("room missing")?;
        assert!(contents_of(room_doc).is_empty());
        assert!(
            room_doc
                .get(automerge::ROOT, "contents")?
                .and_then(|(_, contents)| room_doc
                    .get(&contents, contents_key(compose_id(MEMBER, 1)))
                    .ok()
                    .flatten())
                .is_none()
        );
        assert_eq!(
            tilo_desktop_model::projection::contents_owners_of(room_doc).get(&MEMBER),
            Some(&hex::encode(caller.device_public_key)),
            "the namespace registry stays"
        );
        assert!(!peer.governance_mut().drop_member_windows(&governor, now)?);

        // The member's device publishes another window: refused.
        let changes = member_windows(&peer, MEMBER, "ssh prod-db")?;
        let refused = peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp { changes, room },
            now,
        );
        assert!(refused.is_err(), "{refused:?}");
        assert!(contents_of(peer.admitted_room().ok_or("room missing")?).is_empty());
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
        const MEMBER: u16 = 42;
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert!(
            peer.governance_mut()
                .claim_topology_namespace(&caller, MEMBER, &governor, now)?
        );
        let room = peer.room().to_owned();
        let changes = member_windows(&peer, MEMBER, "Groceries")?;
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
        let dropped = tilo_desktop_model::projection::drop_contents_except(&mut doc, &|id| {
            tilo_desktop_model::ids::peer_salt_of(id) != MEMBER
        });
        assert_eq!(dropped, 1, "the member's strip, with what it holds");
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp { changes, room },
            now,
        )?;
        assert!(contents_of(peer.admitted_room().ok_or("room missing")?).is_empty());
        Ok(())
    }

    /// Only the governor sets the layout scope: a member's change to it is
    /// refused, whatever it says.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_member_cannot_change_the_rooms_layout_scope() -> TestResult {
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
        let refused = peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(caller.device_public_key),
            crate::NeighbourMsg::SyncResp { changes, room },
            now,
        );
        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(peer.governance().room_layout(), RoomLayout::Personal);
        Ok(())
    }
}
