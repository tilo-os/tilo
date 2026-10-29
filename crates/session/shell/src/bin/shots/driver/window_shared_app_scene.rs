//! A Tilo app's window another member shared with a workspace where each
//! person lays out their own windows, as this device shows it: the same app,
//! opened here on the note the sharer has open, in a tab marked with the
//! sharer's face. Nothing of it is a stream.
use automerge::transaction::Transactable as _;

use super::ShellDriver;
use shell::shell::state::RoomId;
use tilo_desktop_model::projection;
use tilo_desktop_model::window_share::{
    WindowShows, shows_node, window_node, window_share_id, window_shows_share_id,
};

/// This device's topology namespace, and the sharer's.
const SALT: u16 = 0x0B52;
const SURFACE: u64 = 0x5a7f;
const NOTES: &str = "so.tilo.notes";

impl ShellDriver {
    /// The Lounge on this device, with Ana's shared Notes window open in it
    /// at her note, drawn by Notes running here: the room, and the tab.
    fn shared_app_in_lounge(&mut self) -> Option<(RoomId, u64)> {
        let sharer_device = iroh::SecretKey::from_bytes(&[5; 32]).public();
        let sharer_hex = hex::encode(sharer_device.as_bytes());
        let session = tilo_network_actor::behaviour::peer_liveness_handle(&sharer_device);
        // Ana's window, minted under the salt her device's key derives.
        let sharer_salt = tilo_desktop_model::ids::device_salt_of_key(sharer_device.as_bytes());
        let tab = (u64::from(sharer_salt) << 48) | 4;
        let mut found = None;
        self.with_chrome(|st| {
            let mut tree = st.tree_mut();
            tree.set_peer_salt(SALT);
            let Some(workspace) = tree.desktop.active_workspace_mut() else {
                return;
            };
            workspace.name = "Lounge".into();
            // Nothing of this person's own is open in the Lounge yet.
            workspace.strips.clear();
            for (session, id, name) in [(session, "ana", "Ana Lima"), (602, "bo", "Bo Jensen")] {
                let mut person = shell::shell::state::Collaborator::new(id, name);
                person.session = Some(session);
                workspace.collaborators.push(person);
            }
            let room = workspace.room();
            let room_text = room.to_string();
            let mut doc = automerge::AutoCommit::new();
            let _ = doc.put(automerge::ROOT, projection::ROOM_ID_KEY, room_text.as_str());
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
                let _ = doc.put(&identities, sharer_hex.as_str(), "{}");
            }
            // Ana's share of her Notes window, at her note.
            let shows = WindowShows {
                record: String::new(),
                item: Some("agenda".into()),
            };
            let mut policy = projection::room_policy_of(&doc, &room_text);
            for (id, node) in [
                (
                    window_shows_share_id(tab),
                    shows_node(&room_text, NOTES, tab, "notes.notebook", &shows),
                ),
                (window_share_id(tab), window_node(&room_text, NOTES, tab)),
            ] {
                if let Ok(grants) = tilo_action_policy::share::share_grants(
                    &room_text,
                    "ana",
                    &id,
                    &node,
                    tilo_action_policy::share::ShareAccess::View,
                ) {
                    policy.grants.extend(grants);
                }
            }
            let _ = projection::project_room_policy(&mut doc, &policy);
            doc.commit();
            let _ = tree.project_room_document_for_test(room, &doc);
            // Notes opened here for the tab.
            shell::shell::tab_binding::adopt_toplevel_at(
                &mut tree,
                SURFACE,
                Some(NOTES.into()),
                shell::shell::tab_binding::Placement::IntoTab(tab),
            );
            tree.package_launches.insert(tab, NOTES.into());
            // Notes names its window after the note it shows.
            let _ = shell::shell::tab_binding::rename_tab_for_title(&mut tree, SURFACE, "Agenda");
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
            drop(tree);
            st.bump();
            found = Some((room, tab));
        });
        let (room, tab) = found?;
        // The tab wears Ana's face, as the frame marks shared windows.
        let _ = self.fb.pump_peer_presence();
        let _ = self.paint_settled();
        std::thread::sleep(std::time::Duration::from_millis(600));
        let _ = self.paint_settled();
        Some((room, tab))
    }

    /// Ana's shared Notes window as this device shows it: Notes running
    /// here at her note, its tab wearing her face.
    pub(super) fn scene_window_shared_app(&mut self) {
        let Some((_, tab)) = self.shared_app_in_lounge() else {
            return;
        };
        self.move_cursor(vello::kurbo::Point::new(800.0, 600.0));
        self.step_frames(120);
        let _ = self.paint_settled();
        let shown = self.chrome().tree().desktop.workspaces.iter().any(|ws| {
            ws.strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .any(|t| {
                    t.id == tab
                        && matches!(
                            &t.tile,
                            shell::shell::state::TileContent::Foreign { app_id, surface_id }
                                if app_id == NOTES && *surface_id == SURFACE
                        )
                })
        });
        assert!(
            shown,
            "window-shared-app scene: the shared Notes window opens here as Notes"
        );
    }

    /// A Notes window in the Lounge, with its History open, while this
    /// device holds a write to the room that no host has kept and the room
    /// tells the window so.
    pub(super) fn scene_window_held_write(&mut self) {
        if self.shared_app_in_lounge().is_none() {
            return;
        }
        let told = ui::floating_surface::FloatingSurfaceHostMessage::Appearance {
            theme: None,
            network_status: None,
            workspace_hue: None,
            your_colour: None,
            room_sync: Some(ui::room_sync::RoomSync {
                state: ui::room_sync::RoomSyncState::Synced,
                workspace: "Lounge".into(),
                writable: Some(true),
                unsynced_writes: 1,
                cannot_keep_writes: false,
            }),
            corner_radius: None,
        };
        for app in &mut self.own_process {
            app.tell(&told);
        }
        // The window says how its changes stand in its History bar: open
        // it from the window's History button.
        self.step_frames(30);
        self.click_at(vello::kurbo::Point::new(1481.0, 57.0));
        self.move_cursor(vello::kurbo::Point::new(800.0, 600.0));
        self.step_frames(120);
        let _ = self.paint_settled();
    }
}
