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
        let Ok((personal, governor)) = owner.with_current(room, |view| {
            Ok((
                view.peer().governance().room_layout().is_personal(),
                Governor::of(view, room),
            ))
        }) else {
            return;
        };
        let governed_here = governor.is_this_device();
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

/// Who governs a room, as far as writing this device's layout into it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Governor {
    /// Another device than this one.
    elsewhere: bool,
    /// A host: a relay, or the room host of the device the room is placed
    /// on, this one's included.
    host: bool,
}

impl Governor {
    fn of(view: &tilo_joined_room::NativeCommittedView, room: RoomId) -> Self {
        Self {
            elsewhere: view.governor().device_public_key != view.device().device_public_key,
            host: view.governor().capabilities.can_act_as_host
                || super::relay_rooms::hosting_governor(view, room)
                    .is_ok_and(|host| host.is_some()),
        }
    }

    /// Whether this device governs the room itself, as no host, so its
    /// layout is admitted here. A host, this device's own room host
    /// included, takes it as a member's, sent like any other device's.
    const fn is_this_device(self) -> bool {
        !self.elsewhere && !self.host
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

    /// Only a room this device governs itself, as no host, admits its
    /// layout here; every host, this device's own room host included, is
    /// sent it as any member's.
    #[test]
    fn only_a_room_this_device_governs_itself_admits_its_layout_here() {
        let here = |elsewhere, host| Governor { elsewhere, host }.is_this_device();
        assert!(!here(false, true), "this device's own host");
        assert!(!here(true, true), "another device's host or a relay");
        assert!(!here(true, false), "another device");
        assert!(here(false, false), "this device governing it itself");
    }
}
