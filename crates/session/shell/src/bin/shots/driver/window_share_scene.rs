//! Sharing a window with a workspace where each person lays out their own
//! windows: the window menu offering it, and the window's bar once the
//! room's host recorded the share.
use automerge::transaction::Transactable as _;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::Widget as _;

use super::ShellDriver;
use shell::shell::state::RoomId;
use tilo_desktop_model::projection;

/// This device's topology namespace in the scenes.
/// The salt the sharer's device key (`[0x5a; 32]`) mints under.
const SALT: u16 = tilo_desktop_model::ids::device_salt_of_key(&[0x5a; 32]);
const SURFACE: u64 = 0x5a7e;
const APP: &str = "Alacritty";

impl ShellDriver {
    /// The active workspace as the Lounge, whose layout is each person's
    /// own, with Ada and Bo in it and a terminal window of this device's in
    /// front: the room, its document as the host keeps it, and the tab.
    fn personal_lounge_window(&mut self) -> Option<(RoomId, automerge::AutoCommit, u64)> {
        let mut found = None;
        self.with_chrome(|st| {
            let mut tree = st.tree_mut();
            tree.set_peer_salt(SALT);
            let Some(workspace) = tree.desktop.active_workspace_mut() else {
                return;
            };
            workspace.name = "Lounge".into();
            for (session, id, name) in [(601, "ada", "Ada Lovelace"), (602, "bo", "Bo Jensen")] {
                let mut person = shell::shell::state::Collaborator::new(id, name);
                person.session = Some(session);
                workspace.collaborators.push(person);
            }
            let room = workspace.room();
            let tab =
                shell::shell::tab_binding::adopt_toplevel(&mut tree, SURFACE, Some(APP.into()));
            let _ = shell::shell::tab_binding::rename_tab_for_title(
                &mut tree,
                SURFACE,
                "~/src/tilo: cargo build",
            );
            let placed = tree.desktop.active_workspace().and_then(|ws| {
                ws.strips.iter().find_map(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .find(|group| group.tabs.iter().any(|t| t.id == tab))
                        .map(|group| (strip.id, group.id))
                })
            });
            if let Some((strip, group)) = placed {
                tree.desktop.set_active_group(strip, group);
            }
            let mut doc = automerge::AutoCommit::new();
            let _ = doc.put(automerge::ROOT, projection::ROOM_ID_KEY, room.to_string());
            let _ = doc.put(
                automerge::ROOT,
                projection::ROOM_LAYOUT_KEY,
                projection::RoomLayout::Personal.id(),
            );
            if let Ok(identities) = doc.put_object(
                automerge::ROOT,
                "peer_identities_v1",
                automerge::ObjType::Map,
            ) {
                let _ = doc.put(&identities, hex::encode([0x5a; 32]), "{}");
            }
            doc.commit();
            let _ = tree.project_room_document_for_test(room, &doc);
            drop(tree);
            st.bump();
            found = Some((room, doc, tab));
        });
        // The new window arrives and settles.
        let _ = self.paint_settled();
        std::thread::sleep(std::time::Duration::from_millis(600));
        let _ = self.paint_settled();
        found
    }

    /// The window menu of a terminal window in the Lounge, opened by the
    /// keyboard, with "Share with Lounge" armed.
    pub(super) fn scene_window_share_menu(&mut self) {
        if self.personal_lounge_window().is_none() {
            return;
        }
        self.set_modifiers(ModifiersState::SUPER);
        self.press_key(&Key::Named(NamedKey::F10));
        self.set_modifiers(ModifiersState::empty());
        assert!(
            self.host().tab_context_menu_is_open(),
            "window-share-menu scene: the shortcut opened no window menu"
        );
        let armed = |driver: &Self| {
            driver
                .host()
                .accessibility()
                .and_then(|root| {
                    root.children
                        .into_iter()
                        .find(|node| node.role == ui::a11y::Role::Menu)
                })
                .and_then(|menu| {
                    menu.children
                        .into_iter()
                        .find(|node| node.state.focused)
                        .and_then(|node| node.name)
                })
        };
        for _ in 0..24 {
            if armed(self).as_deref() == Some("Share with Lounge") {
                break;
            }
            self.press_key(&Key::Named(NamedKey::ArrowDown));
        }
        assert_eq!(
            armed(self).as_deref(),
            Some("Share with Lounge"),
            "window-share-menu scene: the window menu offers Share with Lounge"
        );
        self.step_frames(20);
    }

    /// The same window once the room's host recorded its share: the bar
    /// says "Shared with Lounge" with the faces of Ada and Bo.
    pub(super) fn scene_window_shared(&mut self) {
        let Some((room, mut doc, tab)) = self.personal_lounge_window() else {
            return;
        };
        let room_text = room.to_string();
        let mut policy = projection::room_policy_of(&doc, &room_text);
        if let Ok(grants) = tilo_action_policy::share::share_grants(
            &room_text,
            "me",
            &tilo_desktop_model::window_share::window_share_id(tab),
            &tilo_desktop_model::window_share::window_node(&room_text, APP, tab),
            tilo_action_policy::share::ShareAccess::View,
        ) {
            policy.grants.extend(grants);
        }
        let _ = projection::project_room_policy(&mut doc, &policy);
        doc.commit();
        self.with_chrome(|st| {
            let _ = st.tree_mut().project_room_document_for_test(room, &doc);
            st.bump();
        });
        // The person is working in the window, not pointing at its bar.
        self.move_cursor(vello::kurbo::Point::new(800.0, 600.0));
        self.step_frames(120);
        let _ = self.paint_settled();
        assert!(
            self.host().tab_shared_with(tab).is_some(),
            "window-shared scene: the bar marks the shared window"
        );
    }
}
