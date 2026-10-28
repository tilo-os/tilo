//! The invite and join sheet's scenes: reached the way a person reaches
//! them, with a stand-in network where the sheet needs one.
use winit::keyboard::{Key, NamedKey};

use super::ShellDriver;

impl ShellDriver {
    /// The invite card (`peer_sync_overlay.rs`), reached the way a person
    /// reaches it — the roster's Invite button through the production event
    /// path — so the host wires the invite context exactly as it would live.
    pub(super) fn scene_peer_sync(&mut self) {
        self.scene_collaborators();
        let _ = self.paint_settled();
        let invite = self.host().collaborators_invite_rect_for_tests();
        // A missing Invite button (peer sync off, or the local user may not
        // invite) fails the scene instead of quietly capturing the roster
        // under the name `peer-sync`.
        assert!(
            invite.area() > 0.0,
            "peer-sync scene: the roster offers no Invite button to open the invite card from"
        );
        self.click_at(invite.center());
        // Past the sheet's entrance.
        self.step_frames(20);
    }

    /// Focus the invite sheet's Who field as a person does: by clicking it.
    fn click_invite_field(&mut self) {
        let who = self
            .host()
            .peer_sync_prompt_rects_for_tests()
            .into_iter()
            .find_map(|(name, rect)| (name == "peer-sync-who").then_some(rect));
        assert!(who.is_some(), "the invite sheet has no Who field to click");
        let Some(who) = who else {
            return;
        };
        self.click_at(who.center());
        self.step_frames(2);
    }

    /// Stand a network in for the invite sheet that has invited nobody yet
    /// but `pending`, as a session with peer sync running does.
    fn connect_peer_sync_for_shots(
        &mut self,
        pending: Vec<shell::shell::overlay_host::PendingInvitationSummary>,
    ) {
        use shell::shell::overlay_host::{PeerSyncRequest, PeerSyncResponse};
        self.host_mut()
            .set_peer_sync_bridge(std::rc::Rc::new(move |request| match request {
                PeerSyncRequest::PrepareInvitation => Ok(PeerSyncResponse::Message(String::new())),
                PeerSyncRequest::ListPendingInvitations => {
                    Ok(PeerSyncResponse::PendingInvitations(pending.clone()))
                }
                _ => Err("not in a screenshot".into()),
            }));
    }

    /// The invite sheet as it opens with the network on: nobody named yet.
    pub(super) fn scene_invite_empty(&mut self) {
        self.connect_peer_sync_for_shots(Vec::new());
        self.scene_peer_sync();
    }

    /// The invite sheet while a name is typed: the people this computer
    /// knows whose name holds it, offered under the field.
    pub(super) fn scene_invite_typing(&mut self) {
        use shell::shell::overlay_host::RememberedInvitee;
        self.connect_peer_sync_for_shots(Vec::new());
        self.host_mut().remember_invitees_for_shots(vec![
            RememberedInvitee {
                name: "Ada Lovelace".into(),
                user_public_key: [0x41; 32],
                verification: "Verified in person".into(),
            },
            RememberedInvitee {
                name: "Adrian Moreau".into(),
                user_public_key: [0x42; 32],
                verification: "Verified on 2 Sep".into(),
            },
            RememberedInvitee {
                name: "Bo Lindqvist".into(),
                user_public_key: [0x43; 32],
                verification: "Verified yesterday".into(),
            },
        ]);
        self.scene_peer_sync();
        self.click_invite_field();
        self.type_str("Ad");
        self.step_frames(12);
    }

    /// The invite sheet saying what is missing: a name with no identity
    /// to bind the invite to.
    pub(super) fn scene_invite_error(&mut self) {
        self.connect_peer_sync_for_shots(Vec::new());
        self.scene_peer_sync();
        self.click_invite_field();
        self.type_str("Lin");
        self.press_key(&Key::Named(NamedKey::Enter));
        self.step_frames(12);
    }

    /// The invite sheet with invites still waiting to be used.
    pub(super) fn scene_invite_pending(&mut self) {
        use shell::shell::overlay_host::PendingInvitationSummary;
        let now = tilo_clock::unix_now();
        self.connect_peer_sync_for_shots(vec![
            PendingInvitationSummary {
                id: [1; 32],
                name: "Ada Lovelace".into(),
                role: shell::network::invite::InviteRole::Editor,
                expiry_unix_seconds: now.saturating_add(45 * 60),
            },
            PendingInvitationSummary {
                id: [2; 32],
                name: "Bo Lindqvist".into(),
                role: shell::network::invite::InviteRole::Viewer,
                expiry_unix_seconds: now.saturating_add(20 * 60),
            },
        ]);
        self.scene_peer_sync();
    }

    /// The join sheet as it opens: an empty field and the two ways to
    /// fill it.
    pub(super) fn scene_join_empty(&mut self) {
        self.fb.host_mut().prompt_join_for_shots("");
        self.step_frames(20);
    }
}
