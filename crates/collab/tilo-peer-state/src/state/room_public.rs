//! Whether the room is public, which its governor alone writes, and what a
//! public room never holds: its members' windows.
//!
//! A public room lets strangers in through a door, so it shares only its
//! apps' records. Its governor marks it
//! ([`tilo_desktop_model::projection::ROOM_PUBLIC_KEY`]) for every member to
//! read, commits no member's windows, and removes the ones it committed
//! before it knew.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{ROOT, SystemTime, err};
use automerge::transaction::Transactable as _;
use ed25519_dalek::SigningKey;
use tilo_desktop_model::ids::peer_salt_of;
use tilo_desktop_model::projection::{
    ROOM_PUBLIC_KEY, contents_owners_of, drop_contents_except, room_is_public,
};

impl PeerGovernance<'_> {
    /// Whether the room is public, as its governance document records it.
    #[must_use]
    pub fn room_public(self) -> bool {
        self.peer
            .core
            .room_doc
            .as_deref()
            .is_some_and(room_is_public)
    }
}

impl PeerGovernanceMut<'_> {
    /// Mark the room public, authored by the governor `key`. Marking a public
    /// room changes nothing. Returns whether it changed the room.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer or the change is
    /// refused.
    pub fn author_room_public(self, key: &SigningKey, now: SystemTime) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        if self.peer.governance().room_public() {
            return Ok(false);
        }
        let mut doc = self.peer.core.room_doc.clone().ok_or("missing room")?;
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        doc.put(ROOT, ROOM_PUBLIC_KEY, true).map_err(err)?;
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
        EmptyStrips, contents_key, contents_of, project_owned_contents,
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
                    app_id: "Alacritty".into(),
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

    /// A member's windows the governor committed into a public room are
    /// removed by the governor, and its own namespace's entries stay; the
    /// room is marked public once.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn the_governor_marks_a_public_room_and_drops_its_members_windows() -> TestResult {
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert!(!peer.governance().room_public());
        assert!(peer.governance_mut().author_room_public(&governor, now)?);
        assert!(peer.governance().room_public());
        assert!(!peer.governance_mut().author_room_public(&governor, now)?);

        const MEMBER: u16 = 42;
        assert!(
            peer.governance_mut()
                .claim_topology_namespace(&caller, MEMBER, &governor, now)?
        );
        // The member's windows, committed as the governor took them in
        // before public rooms refused them.
        let mut doc = peer.admitted_room().ok_or("room missing")?.clone();
        let before = doc.get_heads();
        let mut strip = Strip::new(StripId(compose_id(MEMBER, 1)), "Terminals");
        strip.window_groups.push(window(MEMBER, 2, "~/private"));
        project_owned_contents(&mut doc, &[strip], MEMBER, None, EmptyStrips::Skip)?;
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
        let held = contents_of(peer.admitted_room().ok_or("room missing")?);
        assert_eq!(held.len(), 1, "the member's strip was admitted: {held:?}");

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
        assert!(peer.governance().room_public());
        assert!(!peer.governance_mut().drop_member_windows(&governor, now)?);
        Ok(())
    }
}
