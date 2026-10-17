//! Person cursor integration. Geometry comes from the exact peer pointer
//! list the desktop paints, identity from the governed certified roster.

use tilo_desktop::person_cursor::PersonCursor;
use vello::kurbo::{Point, Rect};

use crate::shell::person_quick_composer::PersonDraftOwner;

impl super::ShellOverlayHost {
    pub(crate) fn person_quick_composer_open(&self) -> bool {
        self.person_quick_composer.is_open()
    }

    pub(crate) fn person_marker_pointer_at(&self, point: (f64, f64)) -> bool {
        !self.person_cursor_controls_suppressed()
            && self.person_markers.pointer_at(Point::new(point.0, point.1))
    }

    pub(super) fn person_cursor_controls_suppressed(&self) -> bool {
        self.person_quick_composer.is_open()
            || self.agent_quick_composer.is_open()
            || self.onboarding_active()
            || self.inner_surface_captures_input()
            || self.modal_sheet_owns_keyboard()
    }

    /// Select only actual visible human pointers. A transport session handle
    /// is joined to its admitted user-root principal, never decoded as a key
    /// or matched by display name. Multiple devices yield one person target.
    pub(super) fn painted_person_cursors(
        &self,
        presence: &ui::PresenceCtx,
        theme: &ui::Theme,
        peers: &[ui::presence::PeerCursor],
    ) -> Vec<PersonCursor> {
        let Some(chrome) = &self.chrome else {
            return Vec::new();
        };
        let state = chrome.borrow();
        let local = state.active_local_user();
        let tree = state.tree();
        let Some(workspace) = tree.desktop.active_workspace() else {
            return Vec::new();
        };
        let room = workspace.room.to_string();
        let mut principals = tree.room_peer_principals(workspace.room);
        principals.retain(|_, person| {
            workspace.role_of(person).is_some() && !workspace.is_blocked(person)
        });
        drop(tree);
        drop(state);
        let key = ui::PresenceKey::Workspace { id: room.clone() };
        let Some(pointers) = presence.pointers.get(&key) else {
            return Vec::new();
        };
        pointers
            .iter()
            .zip(peers)
            .filter_map(|((user, _), peer)| {
                if peer.is_agent
                    || peer.opacity <= 0.0
                    || !self.rect.contains(Point::new(peer.pos.0, peer.pos.1))
                {
                    return None;
                }
                let session = user.0.strip_prefix("peer@")?.parse::<u64>().ok()?;
                let person = principals.get(&session)?;
                if person == &local {
                    return None;
                }
                let extent = f64::from(theme.tokens.metrics.bar_control_height);
                let marker = Rect::new(
                    peer.pos.0,
                    peer.pos.1,
                    peer.pos.0 + extent,
                    peer.pos.1 + extent,
                );
                let cursor = PersonCursor {
                    workspace: room.clone(),
                    person_key: person.clone(),
                    label: peer.label.clone(),
                    marker,
                };
                cursor.valid().then_some(cursor)
            })
            .collect()
    }

    pub(super) fn follow_person_composer_anchor(&mut self, cursors: &[PersonCursor]) {
        self.validate_person_composer();
        if !self.person_quick_composer.is_open() {
            return;
        }
        let Some(owner) = self.person_quick_composer.owner() else {
            return;
        };
        if let Some(cursor) = cursors.iter().find(|cursor| {
            cursor.workspace == owner.workspace && cursor.person_key == owner.person_key
        }) {
            self.person_quick_composer.follow_anchor(cursor.marker);
        }
    }

    fn person_cursor_owner(&self, cursor: &PersonCursor) -> Option<PersonDraftOwner> {
        if !cursor.valid() {
            return None;
        }
        let chrome = self.chrome.as_ref()?.borrow();
        let caller = chrome.active_local_user();
        let tree = chrome.tree();
        let workspace = tree.desktop.active_workspace()?;
        if workspace.room.to_string() != cursor.workspace
            || caller == cursor.person_key
            || workspace.role_of(&cursor.person_key).is_none()
            || workspace.is_blocked(&cursor.person_key)
        {
            return None;
        }
        if !tree
            .room_peer_principals(workspace.room)
            .values()
            .any(|person| person == &cursor.person_key)
        {
            return None;
        }
        drop(tree);
        drop(chrome);
        let source = self.marker_action_session_source()?;
        let revision = source.identity_revision(&cursor.workspace)?;
        // Opens the ordinary host-person door, never an app/agent caller or
        // an ambient shell admin. Failure still leaves the draft untouched.
        source.open(&cursor.workspace, Some(&caller)).ok()?;
        drop(source);
        Some(PersonDraftOwner {
            workspace: cursor.workspace.clone(),
            person_key: cursor.person_key.clone(),
            caller,
            identity_revision: revision,
        })
    }

    pub(super) fn pump_person_cursor_activation(&mut self) -> bool {
        let activation = self.person_markers.take_activation();
        if self.person_cursor_controls_suppressed() {
            return false;
        }
        let Some(cursor) = activation else {
            return false;
        };
        let Some(owner) = self.person_cursor_owner(&cursor) else {
            return false;
        };
        self.agent_quick_composer.dismiss();
        self.person_quick_composer
            .open(owner, &cursor.label, cursor.marker, self.overlay_area());
        if let Some(source) = self.marker_action_session_source() {
            self.person_quick_composer.connect(source);
        }
        true
    }

    pub(super) fn validate_person_composer(&mut self) -> bool {
        if !self.person_quick_composer.is_open() {
            return false;
        }
        if self.agent_quick_composer.is_open() {
            self.person_quick_composer.dismiss();
            return true;
        }
        let valid = self.person_quick_composer.owner().is_some_and(|owner| {
            let Some(chrome) = &self.chrome else {
                return false;
            };
            let state = chrome.borrow();
            if state.active_local_user() != owner.caller {
                return false;
            }
            let tree = state.tree();
            let Some(workspace) = tree.desktop.active_workspace() else {
                return false;
            };
            workspace.room.to_string() == owner.workspace
                && workspace.role_of(&owner.caller).is_some()
                && !workspace.is_blocked(&owner.caller)
                && workspace.role_of(&owner.person_key).is_some()
                && !workspace.is_blocked(&owner.person_key)
                && tree
                    .room_peer_principals(workspace.room)
                    .values()
                    .any(|person| person == &owner.person_key)
                && self
                    .marker_action_session_source()
                    .and_then(|source| source.identity_revision(&owner.workspace))
                    == Some(owner.identity_revision)
        });
        if !valid {
            self.person_quick_composer.dismiss();
        }
        !valid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;
    use vello::{Scene, kurbo::Affine};

    #[test]
    fn only_painted_certified_remote_people_publish_chat_targets() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.adopt_ownerless_workspaces(&"owner".into());
        let room = tree.desktop.active_workspace().unwrap().room;
        let endpoint = iroh::SecretKey::generate().public();
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[83; 32]);
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: signing_key.verifying_key().to_bytes(),
            device_public_key: *endpoint.as_bytes(),
            device_label: "Remote laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &signing_key).unwrap();
        assert!(tree.name_peer(room, &"owner".into(), &endpoint.to_string(), "Ada"));
        assert!(tree.certify_peer_identity(room, &endpoint.to_string(), certificate));
        tree.desktop.workspaces[0].admit(
            hex::encode(signing_key.verifying_key().to_bytes()),
            crate::shell::state::CapRole::Editor,
        );
        let handle = crate::network::behaviour::peer_liveness_handle(&endpoint);
        let user = ui::permissions::UserId(format!("peer@{handle}"));
        let target = format!(
            "participant:{}:chat",
            hex::encode(signing_key.verifying_key().to_bytes())
        );
        let key = ui::PresenceKey::Workspace {
            id: room.to_string(),
        };
        let (mut host, _) = crate::shell::build_tilo_overlay_host_with_chrome(&tree);
        host.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut theme = ui::Theme::dark();
        theme.reduce_motion = true;
        let mut presence = ui::PresenceCtx::default();
        let mut text = ui::text_render::TextRenderer::new();
        let mut scene = Scene::new();
        host.draw_real_peers(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        assert_eq!(
            host.person_markers.children().count(),
            0,
            "roster alone has no cursor"
        );
        presence
            .pointers
            .insert(key.clone(), vec![(user.clone(), (400.0, 300.0))]);
        host.draw_real_peers(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        assert!(host.person_markers.actionable(&target));
        let node = host
            .person_markers
            .children()
            .next()
            .unwrap()
            .accessibility()
            .unwrap();
        assert_eq!(node.action_target.as_deref(), Some(target.as_str()));

        assert_native_confirmation_suppresses_person_cursor(&mut host, &target);

        presence.set_participant_kind(user.clone(), true);
        host.draw_real_peers(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        assert!(
            !host.person_markers.actionable(&target),
            "an Agent is not a person cursor"
        );
        presence.set_participant_kind(user.clone(), false);
        presence
            .pointers
            .insert(key.clone(), vec![(user, (1400.0, 300.0))]);
        host.draw_real_peers(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        assert!(
            !host.person_markers.actionable(&target),
            "an offscreen edge mist is not a cursor target"
        );
        presence.pointers.insert(
            key,
            vec![(ui::permissions::UserId("peer@1".into()), (400.0, 300.0))],
        );
        host.draw_real_peers(&mut scene, Affine::IDENTITY, &mut text, &theme, &presence);
        assert_eq!(
            host.person_markers.children().count(),
            0,
            "uncertified handles cannot invent recipients"
        );
    }

    fn assert_native_confirmation_suppresses_person_cursor(
        host: &mut super::super::ShellOverlayHost,
        target: &str,
    ) {
        // A native confirmation can arrive after the cursor was painted.
        // Both pointer routing and the semantic door must use the fresh
        // sheet state, not the previous frame's enabled cursor control.
        let queue = crate::shell::portal_dialog::PortalDialogQueue::default();
        let mut answer = queue
            .enqueue_review(crate::shell::portal_dialog::ReviewRequest {
                handle: "person-cursor-confirmation".into(),
                app_id: "so.tilo.chat".into(),
                title: "Confirm action".into(),
                reason: "Review the requested action".into(),
                resource: "Conversation".into(),
                confirm_label: "Confirm".into(),
            })
            .unwrap();
        host.set_portal_dialog_queue(queue);
        assert!(host.pump_portal_dialogs());
        assert!(host.modal_sheet_owns_keyboard());
        assert!(!host.person_marker_pointer_at((410.0, 310.0)));
        assert!(ui::widget::Widget::perform_semantic_action(
            host,
            target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert!(host.person_markers.take_activation().is_none());
        host.portal_dialogs.confirm();
        assert_eq!(
            answer.try_recv().unwrap(),
            crate::shell::portal_dialog::ReviewDecision::Confirmed
        );
        assert!(!host.modal_sheet_owns_keyboard());
        assert!(!host.person_cursor_controls_suppressed());
    }
}
