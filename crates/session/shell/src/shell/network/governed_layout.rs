//! This device's layout of a room another device governs, or that this
//! device governs through its native owner.
//!
//! Every member writes the room's layout as its own changes
//! ([`crate::shell::state::WorkspaceTree::capture_governed_layout`]): the
//! windows it opened, and wherever it moved, resized or closed any window,
//! one another device runs included. The governor admits them by what each
//! change does to what someone else made, and every member takes them in
//! from it. A change waits here, shown on this device, until the room holds
//! it: whether the governor, or the device a window runs on, is online
//! changes when the others see it, never whether it sticks.
use super::{NetCmd, ShellNetwork, room_topic_id};
use crate::shell::state::RoomId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often one room's layout is looked at.
const CHECK_EVERY: Duration = Duration::from_millis(500);
/// Changes the governor has not admitted yet are sent again after this long.
const RESEND_AFTER: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Room {
    checked: Option<Instant>,
    sent_at: Option<Instant>,
    /// The admitted room document's heads when a room whose layout is
    /// personal was last looked at: while they hold, there is nothing of
    /// this device's to take out of it.
    personal_heads: Option<Vec<automerge::ChangeHash>>,
}

#[derive(Default)]
pub(super) struct State {
    rooms: Arc<Mutex<HashMap<RoomId, Room>>>,
}

impl ShellNetwork {
    /// The peers a headless session's viewers watch `room` through: its
    /// workspace hosts, and the device governing the room, which forwards
    /// a controlling viewer's input as its own. Each is shared every legacy
    /// window of the room's workspace while the session is watched. Called
    /// every tick, so it reads the governor only from an owner already open.
    pub fn viewer_hosts(&self, room: RoomId) -> std::collections::HashSet<u64> {
        let mut hosts = self.workspace_hosts(room_topic_id(room));
        if let Some(owner) = self.opened_native_owner()
            && let Ok(governor) = owner.with_current(room, |view| {
                Ok((
                    view.device().device_public_key,
                    view.governor().device_public_key,
                ))
            })
            && governor.0 != governor.1
            && let Ok(endpoint) = iroh::EndpointId::from_bytes(&governor.1)
        {
            hosts.insert(crate::network::behaviour::peer_liveness_handle(&endpoint));
        }
        hosts
    }

    /// Keep `room`'s layout showing this device's: what it changed since
    /// the room last took it in is written as its own changes and sent to
    /// the governor (admitted here when this device governs the room), again
    /// until the room holds them. Nothing in a room whose layout is
    /// personal. Cheap when nothing changed. Call every tick.
    pub fn publish_governed_layout(
        &self,
        room: RoomId,
        tree: &crate::shell::state::WorkspaceTreeHandle,
    ) {
        let now = Instant::now();
        {
            let Ok(mut rooms) = self.governed_layout.rooms.lock() else {
                return;
            };
            let state = rooms.entry(room).or_default();
            if state
                .checked
                .is_some_and(|at| now.saturating_duration_since(at) < CHECK_EVERY)
            {
                return;
            }
            state.checked = Some(now);
        }
        // Only an owner already open: this runs on the frame loop.
        let Some(owner) = self.opened_native_owner() else {
            return;
        };
        let (governed_here, personal) = owner
            .with_current(room, |view| {
                Ok((
                    view.governed_here(),
                    view.peer().governance().room_layout().is_personal(),
                ))
            })
            .unwrap_or((false, false));
        if !governed_here && !self.governed_elsewhere_by(&owner, room) {
            return;
        }
        // A room whose layout is personal takes no member's windows: none is
        // written into it, and nothing written before is sent again. Its
        // governor removes what it took in of them while it shared them:
        // when that is this device, its own too.
        if personal {
            self.keep_out_of_personal_room(owner, room, governed_here, tree, now);
            return;
        }
        let pending = {
            let mut tree = tree.write();
            tree.capture_governed_layout(room);
            tree.pending_layout(room)
        };
        if pending.is_empty() {
            return;
        }
        if governed_here {
            self.admit_own_topology(owner, room, pending, now);
        } else {
            self.send_topology(room, pending, now);
        }
    }

    /// Write nothing into `room`, whose layout is personal: when this device
    /// governs it, take its own windows out of it too, once per change of
    /// the room's document.
    fn keep_out_of_personal_room(
        &self,
        owner: crate::shell::native_workspace_owner::NativeWorkspaceOwner,
        room: RoomId,
        governed_here: bool,
        tree: &crate::shell::state::WorkspaceTreeHandle,
        now: Instant,
    ) {
        use automerge::transaction::Transactable;
        let heads = owner
            .with_current(room, |view| {
                Ok(view.peer().admitted_room().map(Transactable::base_heads))
            })
            .ok()
            .flatten();
        tree.write().layout_pending.remove(&room);
        let looked = self.governed_layout.rooms.lock().is_ok_and(|mut rooms| {
            let state = rooms.entry(room).or_default();
            let looked = heads.is_some() && state.personal_heads == heads;
            state.personal_heads = heads;
            looked
        });
        if governed_here && !looked {
            let salt = tree.read().local_salt();
            self.take_own_windows_out(owner, room, salt, now);
        }
    }

    /// Take every window this device put into `room`, which it governs and
    /// whose layout is personal, out of the room's layout: its windows stay
    /// on this device. Nothing is done when there are none.
    fn take_own_windows_out(
        &self,
        owner: crate::shell::native_workspace_owner::NativeWorkspaceOwner,
        room: RoomId,
        salt: u16,
        now: Instant,
    ) {
        if salt == 0 {
            return;
        }
        let changes = owner.with_current(room, |view| {
            let mut doc = view
                .peer()
                .admitted_room()
                .ok_or("room governance missing")?
                .clone();
            let heads = doc.get_heads();
            doc.set_actor(automerge::ActorId::from(
                view.device().device_public_key.to_vec(),
            ));
            let dropped = tilo_desktop_model::shared_record::device::withdraw(&mut doc, &|id| {
                tilo_desktop_model::peer_salt_of(id) == salt
            })
            .map_err(|error| error.to_string())?;
            doc.commit();
            Ok(dropped.then(|| {
                doc.get_changes(&heads)
                    .iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect::<Vec<_>>()
            }))
        });
        match changes {
            Ok(Some(changes)) => self.admit_own_topology(owner, room, changes, now),
            Ok(None) => {}
            Err(error) => tracing::debug!(%room, %error, "own windows not taken out"),
        }
    }

    /// Whether changes not yet admitted in `room` are due to go again.
    fn due_to_send(&self, room: RoomId, now: Instant) -> bool {
        let Ok(mut rooms) = self.governed_layout.rooms.lock() else {
            return false;
        };
        let state = rooms.entry(room).or_default();
        if state
            .sent_at
            .is_some_and(|at| now.saturating_duration_since(at) < RESEND_AFTER)
        {
            return false;
        }
        state.sent_at = Some(now);
        true
    }

    /// Admit this device's own layout `changes` into `room`, which it
    /// governs, off the frame loop: admission publishes the room durably.
    fn admit_own_topology(
        &self,
        owner: crate::shell::native_workspace_owner::NativeWorkspaceOwner,
        room: RoomId,
        changes: Vec<Vec<u8>>,
        now: Instant,
    ) {
        if !self.due_to_send(room, now) {
            return;
        }
        match self.runtime.as_ref() {
            Some(runtime) => {
                runtime.spawn_blocking(move || admit_own_topology(&owner, room, changes));
            }
            None => admit_own_topology(&owner, room, changes),
        }
    }

    fn send_topology(&self, room: RoomId, changes: Vec<Vec<u8>>, now: Instant) {
        if !self.due_to_send(room, now) {
            return;
        }
        tracing::debug!(%room, changes = changes.len(), "sending this device's layout to the room");
        let msg = crate::network::NeighbourMsg::SyncResp {
            changes,
            room: room.to_string(),
        };
        let _ = self.cmd_tx.try_send(NetCmd::PublishHostedTopology {
            ws: room_topic_id(room),
            msg,
        });
    }
}

/// Admit this device's own layout changes into a room it governs: they are
/// its governor's, so they land as the governor's own room changes.
fn admit_own_topology(
    owner: &crate::shell::native_workspace_owner::NativeWorkspaceOwner,
    room: RoomId,
    changes: Vec<Vec<u8>>,
) {
    let device = match owner.with_current(room, |view| Ok(view.device().device_public_key)) {
        Ok(device) => device,
        Err(error) => {
            tracing::debug!(%room, %error, "own layout not admitted");
            return;
        }
    };
    if let Err(error) = owner.receive(
        room,
        tilo_peer_state::TransportOrigin::RelayDelta(device),
        crate::network::NeighbourMsg::SyncResp {
            changes,
            room: room.to_string(),
        },
        std::time::SystemTime::now(),
    ) {
        tracing::warn!(%room, %error, "own layout refused by the room this device governs");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Strip, Tab, TileContent, WindowGroup, WindowWidth};

    /// The windows the room's admitted layout holds, in order.
    fn admitted_tabs(
        owner: &crate::shell::native_workspace_owner::NativeWorkspaceOwner,
        room: RoomId,
    ) -> Vec<u64> {
        owner
            .with_current(room, |view| {
                let doc = view.peer().admitted_room().ok_or("no room")?;
                Ok(tilo_desktop_model::shared_record::read(doc, None)
                    .into_iter()
                    .flat_map(|desktop| desktop.tree.workspaces)
                    .flat_map(|workspace| workspace.strips)
                    .flat_map(|strip| strip.window_groups)
                    .flat_map(|group| group.tabs)
                    .map(|tab| tab.id)
                    .collect())
            })
            .unwrap()
    }

    fn tab(id: u64, tile: TileContent) -> Tab {
        Tab {
            id,
            title: format!("tab {id:x}"),
            tile,
            app_title: None,
        }
    }

    /// A device host forwards a viewer's input with the relay's legacy-window
    /// vocabulary and the governor's handle; both must be what this session
    /// decodes.
    #[test]
    fn viewer_input_from_a_device_host_decodes_as_remote_input() {
        use crate::network::streaming::InputEvent as Session;
        use tilo_relay_protocol::legacy_window::InputEvent as Wire;
        let pairs = [
            (
                Session::PointerMotion { x: 1.5, y: -2.0 },
                Wire::PointerMotion { x: 1.5, y: -2.0 },
            ),
            (
                Session::PointerButton {
                    button: 272,
                    pressed: true,
                },
                Wire::PointerButton {
                    button: 272,
                    pressed: true,
                },
            ),
            (
                Session::PointerEnter { surface: 7 },
                Wire::PointerEnter { surface: 7 },
            ),
            (
                Session::PointerLeave { surface: 7 },
                Wire::PointerLeave { surface: 7 },
            ),
            (
                Session::Key {
                    keysym: 30,
                    pressed: false,
                    modifiers: 4,
                },
                Wire::Key {
                    keysym: 30,
                    pressed: false,
                    modifiers: 4,
                },
            ),
            (
                Session::KeyboardFocus {
                    surface: 7,
                    focused: true,
                },
                Wire::KeyboardFocus {
                    surface: 7,
                    focused: true,
                },
            ),
        ];
        for (session, wire) in pairs {
            assert_eq!(
                postcard::to_allocvec(&session).unwrap(),
                postcard::to_allocvec(&wire).unwrap(),
                "{session:?}"
            );
        }
        // The device host's `legacy_windows::peer_handle` pins the same value.
        let key = iroh::SecretKey::from_bytes(&[1; 32]);
        assert_eq!(
            crate::network::behaviour::peer_liveness_handle(&key.public()),
            0x90b3_65ba_582f_856f
        );
    }

    /// A relay forwards a browser's input (wheel, a paste, a close, text its
    /// input method committed among it) in its wire vocabulary; each must be
    /// what this session decodes.
    #[test]
    fn viewer_input_from_a_relay_decodes_as_remote_input() {
        use crate::network::streaming::InputEvent as Session;
        use tilo_relay_protocol::legacy_window::InputEvent as Wire;
        let pairs = [
            (
                Session::PointerMotion { x: 1.5, y: -2.0 },
                Wire::PointerMotion { x: 1.5, y: -2.0 },
            ),
            (
                Session::KeyboardFocus {
                    surface: 7,
                    focused: true,
                },
                Wire::KeyboardFocus {
                    surface: 7,
                    focused: true,
                },
            ),
            (
                Session::PointerAxis { dx: 0.0, dy: 30.0 },
                Wire::PointerAxis { dx: 0.0, dy: 30.0 },
            ),
            (
                Session::ClipboardText {
                    text: "pasted".into(),
                },
                Wire::ClipboardText {
                    text: "pasted".into(),
                },
            ),
            (Session::Close, Wire::Close),
            (
                Session::Text {
                    text: "日本".into(),
                },
                Wire::Text {
                    text: "日本".into(),
                },
            ),
        ];
        for (session, wire) in pairs {
            assert_eq!(
                postcard::to_allocvec(&session).unwrap(),
                postcard::to_allocvec(&wire).unwrap(),
                "{session:?}"
            );
        }
        assert_eq!(
            tilo_relay_protocol::legacy_window::MAX_TEXT_BYTES,
            crate::network::input_firewall::MAX_REMOTE_TEXT_BYTES,
            "a relay forwards no text this session's firewall refuses"
        );
    }

    /// A room this device governs takes every window this device opens into
    /// its layout, admitted here, an unconfined program's too (its members
    /// view it as a stream): no other device places them. Once the room's
    /// layout is personal no window is in it at all.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
    fn a_room_this_device_governs_takes_every_window_it_opens_until_its_layout_is_personal() {
        use crate::shell::state::{StripId, WorkspaceTree};
        use ed25519_dalek::SigningKey;
        let directory = tempfile::tempdir().unwrap();
        let salt = 0x0042;
        let own = |counter: u64| (u64::from(salt) << 48) | counter;
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.set_peer_salt(salt);
        tree.persist_path = Some(directory.path().join("state.json"));
        let workspace = tree.desktop.active;
        let placeholder = TileContent::AppPlaceholder {
            kind: crate::shell::state::AppKind::Other("so.tilo.notes".into()),
            label: "Notes".into(),
        };
        let foreign = TileContent::Foreign {
            app_id: "org.chromium.Chromium".into(),
            surface_id: 7,
        };
        let room = {
            let ws = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|ws| ws.id == workspace)
                .unwrap();
            ws.strips = vec![Strip {
                id: StripId(own(1)),
                name: "Workspace".into(),
                window_groups: vec![WindowGroup {
                    id: own(2),
                    tabs: vec![tab(own(3), placeholder), tab(own(4), foreign)],
                    active_tab: own(3),
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                }],
            }];
            ws.room()
        };
        let tree = tree.shared();
        let owner = crate::shell::native_workspace_owner::open(tree.clone()).unwrap();
        let key = SigningKey::from_bytes(&[71; 32]);
        let device = tilo_joined_room::testing::certificate(
            &SigningKey::from_bytes(&[72; 32]),
            &key,
            "Desktop",
        );
        let seed = tilo_peer_state::workspace_seed(
            &room.to_string(),
            &device,
            std::slice::from_ref(&device),
            "Garden",
        )
        .unwrap();
        owner
            .create_governed(
                room,
                workspace.raw(),
                &device,
                &key,
                seed,
                std::time::SystemTime::now(),
            )
            .unwrap();
        assert!(tree.read().requires_native_owner(room));

        let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
        network.publish_governed_layout(room, &tree);
        let tabs = admitted_tabs(&owner, room);
        assert_eq!(tabs, [own(3), own(4)], "both windows are in the room");
        let shown: Vec<u64> = tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.room() == room)
            .unwrap()
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect();
        assert_eq!(
            shown,
            [own(3), own(4)],
            "and still shown here, as they were"
        );

        let changes = owner
            .with_current(room, |view| {
                let mut doc = view.peer().admitted_room().ok_or("no room")?.clone();
                let heads = doc.get_heads();
                doc.set_actor(automerge::ActorId::from(device.device_public_key.to_vec()));
                automerge::transaction::Transactable::put(
                    &mut doc,
                    automerge::ROOT,
                    tilo_desktop_model::projection::ROOM_LAYOUT_KEY,
                    tilo_desktop_model::projection::RoomLayout::Personal.id(),
                )
                .map_err(|error| error.to_string())?;
                doc.commit();
                Ok(doc
                    .get_changes(&heads)
                    .iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect::<Vec<_>>())
            })
            .unwrap();
        owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RelayDelta(device.device_public_key),
                crate::network::NeighbourMsg::SyncResp {
                    changes,
                    room: room.to_string(),
                },
                std::time::SystemTime::now(),
            )
            .unwrap();
        tree.write()
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.room() == room)
            .unwrap()
            .strips[0]
            .window_groups[0]
            .tabs
            .push(tab(
                own(5),
                TileContent::AppPlaceholder {
                    kind: crate::shell::state::AppKind::Other("so.tilo.canvas".into()),
                    label: "Canvas".into(),
                },
            ));
        let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
        network.publish_governed_layout(room, &tree);
        let tabs = admitted_tabs(&owner, room);
        assert!(
            tabs.is_empty(),
            "a personal room holds none of this device's windows: {tabs:?}"
        );
        let shown = tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.room() == room)
            .unwrap()
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .count();
        assert_eq!(shown, 3, "all three stay on this device");
    }
}
