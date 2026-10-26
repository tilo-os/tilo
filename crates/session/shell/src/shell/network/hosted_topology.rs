//! This session's legacy windows in a room another device governs.
//!
//! Such a room's topology is written by its governor, so a window this
//! session adopted itself (a legacy Wayland or X11 client) is in nobody's
//! room document, and the room's other members, a browser viewer among
//! them, have no tab to show it in. The room's registry lets a member own a
//! topology namespace: once the governor registers this session's salt to
//! this device, the session publishes its legacy tabs as its own entries,
//! which the governor admits within that namespace and republishes.
use super::{NetCmd, ShellNetwork, room_topic_id};
use crate::shell::state::{RoomId, Strip, TileContent};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_desktop_model::commands::{BoundCommand, CommandSource, DesktopCommand};

/// How often one room's legacy windows are looked at.
const CHECK_EVERY: Duration = Duration::from_millis(500);
/// A claim that did not answer is asked again after this long.
const CLAIM_RETRY: Duration = Duration::from_secs(10);
/// Entries the governor has not admitted yet are sent again after this long.
const RESEND_AFTER: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Room {
    checked: Option<Instant>,
    claimed_at: Option<Instant>,
    sent_at: Option<Instant>,
    settled: Option<Settled>,
    /// The changes this session published that the governor has not
    /// admitted yet, oldest first ([`plan`]).
    pending: Vec<Vec<u8>>,
    /// The admitted room document's heads when a room whose layout is
    /// personal was last looked at: while they hold, there is nothing of
    /// this device's to take out of it.
    personal_heads: Option<Vec<automerge::ChangeHash>>,
    /// Retained layout requests this device has already applied and not yet
    /// seen leave the admitted document. Applying one again is harmless, so
    /// an unadmitted change is retried rather than lost.
    retained: BTreeSet<String>,
}

/// What a look at a room last found nothing to do for. The outcome is a
/// function of exactly these, so while they hold the next look would find
/// nothing again, and the room's document is not copied to find that out.
#[derive(PartialEq, Eq)]
struct Settled {
    /// The admitted room document's heads.
    heads: Vec<automerge::ChangeHash>,
    device: [u8; 32],
    salt: u16,
    /// [`own_legacy_windows`], serialised: `Strip` has no `Eq` of its own.
    windows: Vec<u8>,
}

#[derive(Default)]
pub(super) struct State {
    rooms: Arc<Mutex<HashMap<RoomId, Room>>>,
}

/// `strips` cut down to this session's own legacy tabs (and the groups and
/// strips that hold them), the entries it publishes in a governed room.
pub(crate) fn own_legacy_windows(strips: &[Strip], salt: u16) -> Vec<Strip> {
    let own = |id: u64| tilo_desktop_model::peer_salt_of(id) == salt;
    strips
        .iter()
        .filter_map(|strip| {
            let groups = strip
                .window_groups
                .iter()
                .filter_map(|group| {
                    let tabs = group
                        .tabs
                        .iter()
                        .filter(|tab| {
                            own(tab.id) && matches!(tab.tile, TileContent::Foreign { .. })
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    let first = tabs.first()?.id;
                    Some({
                        let mut group = group.clone();
                        if !tabs.iter().any(|tab| tab.id == group.active_tab) {
                            group.active_tab = first;
                        }
                        group.tabs = tabs;
                        group
                    })
                })
                .collect::<Vec<_>>();
            (!groups.is_empty()).then(|| Strip {
                window_groups: groups,
                ..strip.clone()
            })
        })
        .collect()
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

    /// Keep `room`'s document showing this session's windows among its
    /// workspace's layout in `tree`. When another device governs `room`,
    /// that is this session's own legacy tabs (`own_legacy_windows`),
    /// published for the governor to admit; the governor places the rest.
    /// When this device governs `room` itself, it is every window it owns,
    /// admitted here. Nothing at all in a room
    /// whose layout is personal. Cheap when nothing changed: while neither those
    /// windows nor the admitted room document moved since a look found
    /// nothing to publish, the document is not copied. Call every tick.
    pub fn publish_hosted_legacy_windows(
        &self,
        room: RoomId,
        tree: &crate::shell::state::WorkspaceTreeHandle,
    ) {
        let now = Instant::now();
        {
            let Ok(mut rooms) = self.hosted_topology.rooms.lock() else {
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
        // published into it, and nothing published before is sent again.
        // Its governor removes what it took in of them while it shared them:
        // when that is this device, its own too.
        if personal {
            self.keep_out_of_personal_room(owner, room, governed_here, tree, now);
            return;
        }
        // Retained requests for this device's windows are room state, not a
        // write into someone else's namespace: drain, claim and apply each
        // once, in the same change that publishes the result.
        self.apply_retained_layout_requests(&owner, room, tree, governed_here, now);
        let (salt, windows) = windows_to_publish(&tree.read(), room, governed_here);
        if salt == 0 {
            return;
        }
        let (mut settled, mut pending) = self
            .hosted_topology
            .rooms
            .lock()
            .ok()
            .and_then(|mut rooms| {
                let state = rooms.get_mut(&room)?;
                Some((state.settled.take(), std::mem::take(&mut state.pending)))
            })
            .unwrap_or_default();
        let planned = owner.with_current(room, |view| {
            let admitted = view
                .peer()
                .admitted_room()
                .ok_or("room governance missing")?;
            plan(
                admitted,
                Look {
                    device: view.device().device_public_key,
                    windows: &windows,
                    salt,
                },
                &mut settled,
                &mut pending,
            )
        });
        if let Ok(mut rooms) = self.hosted_topology.rooms.lock() {
            let state = rooms.entry(room).or_default();
            state.settled = settled;
            state.pending = pending;
        }
        match planned {
            Ok(Some(Plan::Claim)) if governed_here => {
                tracing::warn!(%room, salt, "this device's namespace is not registered in the room it governs");
            }
            Ok(Some(Plan::Claim)) => self.claim_namespace(room, salt, now),
            Ok(Some(Plan::Publish(changes))) if governed_here => {
                let _ = self.admit_own_topology(owner, room, changes, now);
            }
            Ok(Some(Plan::Publish(changes))) => self.send_topology(room, changes, now),
            Ok(None) => {}
            Err(error) => tracing::debug!(%room, %error, "hosted legacy windows not published"),
        }
    }

    /// Apply the retained requests addressed to this device's windows: read
    /// them from the admitted document, admit each against the exact revision
    /// its requester saw, reduce it in this device's own workspace, and record
    /// the applied id in this device's namespace under the same change that
    /// publishes the rearranged entries. A request this device already
    /// recorded is never applied twice, and a restart reads the record back
    /// from the room.
    fn apply_retained_layout_requests(
        &self,
        owner: &crate::shell::native_workspace_owner::NativeWorkspaceOwner,
        room: RoomId,
        tree: &crate::shell::state::WorkspaceTreeHandle,
        governed_here: bool,
        now: Instant,
    ) {
        use tilo_desktop_model::commands::{BoundCommand, admit, apply_command};
        use tilo_desktop_model::projection::{EmptyStrips, project_owned_contents};
        use tilo_desktop_model::room_layout::{claim, pending};

        let opened = owner
            .with_current(room, |view| {
                Ok(view
                    .peer()
                    .admitted_room()
                    .cloned()
                    .map(|admitted| (admitted, view.device().device_public_key)))
            })
            .ok()
            .flatten();
        let Some((admitted, device)) = opened else {
            return;
        };
        let (salt, share_floor) = {
            let tree = tree.read();
            (tree.local_salt(), tree.room_share_floor.get(&room).copied())
        };
        if salt == 0 {
            return;
        }
        let wanted = pending(&admitted, salt);
        if wanted.is_empty() {
            if let Ok(mut rooms) = self.hosted_topology.rooms.lock() {
                if let Some(state) = rooms.get_mut(&room) {
                    state.retained.clear();
                }
            }
            return;
        }
        let fresh: Vec<_> = {
            let Ok(rooms) = self.hosted_topology.rooms.lock() else {
                return;
            };
            let held = rooms.get(&room).map(|state| &state.retained);
            wanted
                .into_iter()
                .filter(|request| held.is_none_or(|held| !held.contains(&request.id)))
                .collect()
        };
        if fresh.is_empty() {
            return;
        }
        let mut doc = admitted;
        let heads = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(device.to_vec()));
        let mut claimed = Vec::new();
        {
            let mut tree = tree.write();
            let Some(workspace) = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room() == room)
            else {
                return;
            };
            for request in &fresh {
                let Ok(bound) = serde_json::from_slice::<BoundCommand>(&request.command) else {
                    continue;
                };
                let owned = |id| tilo_desktop_model::peer_salt_of(id) == salt;
                let Ok(command) = admit(workspace, &bound.source, &mut doc, &bound.command, owned)
                else {
                    // The window moved on since the request was made. Keep
                    // it retained, not silently dropped: a later look may
                    // still be able to apply it.
                    continue;
                };
                match apply_command(
                    workspace,
                    &command,
                    &mut || Err("a rearranged window mints no identity".into()),
                    &mut |_, _| Err("a rearranged window creates no record".into()),
                ) {
                    Ok(()) => {
                        if claim(&mut doc, salt, request).unwrap_or(false) {
                            claimed.push(request.id.clone());
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%room, request = %request.id, %error, "retained layout request refused");
                    }
                }
            }
            if claimed.is_empty() {
                return;
            }
            let strips = if governed_here {
                workspace.strips.clone()
            } else {
                own_legacy_windows(&workspace.strips, salt)
            };
            if let Err(error) =
                project_owned_contents(&mut doc, &strips, salt, share_floor, EmptyStrips::Skip)
            {
                tracing::debug!(%room, %error, "retained layout projection refused");
                return;
            }
            doc.commit();
            tree.queue_save();
        }
        let changes = doc
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect::<Vec<_>>();
        if changes.is_empty() {
            return;
        }
        if governed_here {
            // A retained request's result is not the periodic topology
            // resend: publish it now rather than losing it to the cadence.
            if let Ok(mut rooms) = self.hosted_topology.rooms.lock() {
                rooms.entry(room).or_default().sent_at = None;
            }
            if !self.admit_own_topology(owner.clone(), room, changes, now) {
                // The room's publication cadence held it; the request is
                // still pending, so the next look applies it again.
                return;
            }
        } else if let Ok(mut rooms) = self.hosted_topology.rooms.lock() {
            let state = rooms.entry(room).or_default();
            state.pending.extend(changes);
            state.settled = None;
        }
        if let Ok(mut rooms) = self.hosted_topology.rooms.lock() {
            let held = &mut rooms.entry(room).or_default().retained;
            held.extend(claimed);
        }
    }

    /// Publish nothing into `room`, whose layout is personal, and forget what
    /// was waiting to be: when this device governs it, take its own windows
    /// out of it too, once per change of the room's document.
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
        let looked = self.hosted_topology.rooms.lock().is_ok_and(|mut rooms| {
            let state = rooms.entry(room).or_default();
            state.pending.clear();
            state.settled = None;
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
    /// whose layout is personal, out of the room's document: its windows
    /// stay on this device. Nothing is done when there are none.
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
            let dropped = tilo_desktop_model::projection::drop_contents_except(&mut doc, &|id| {
                tilo_desktop_model::peer_salt_of(id) != salt
            });
            doc.commit();
            Ok((dropped > 0).then(|| {
                doc.get_changes(&heads)
                    .iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect::<Vec<_>>()
            }))
        });
        match changes {
            Ok(Some(changes)) => {
                let _ = self.admit_own_topology(owner, room, changes, now);
            }
            Ok(None) => {}
            Err(error) => tracing::debug!(%room, %error, "own windows not taken out"),
        }
    }

    fn claim_namespace(&self, room: RoomId, salt: u16, now: Instant) {
        {
            let Ok(mut rooms) = self.hosted_topology.rooms.lock() else {
                return;
            };
            let state = rooms.entry(room).or_default();
            if state
                .claimed_at
                .is_some_and(|at| now.saturating_duration_since(at) < CLAIM_RETRY)
            {
                return;
            }
            state.claimed_at = Some(now);
        }
        let bound = BoundCommand {
            source: CommandSource {
                room: room.to_string(),
                heads: Vec::new(),
            },
            command: DesktopCommand::ClaimNamespace { salt },
        };
        let Ok(command) = serde_json::to_string(&bound) else {
            return;
        };
        let answer = self.claim_namespace_at_host(room, salt).unwrap_or_else(|| {
            self.invoke_governor(
                room,
                tilo_desktop_model::commands::APP_ID,
                tilo_desktop_model::commands::RESOURCE,
                tilo_desktop_model::commands::ACTION,
                Some(serde_json::json!({ "command": command })),
            )
        });
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        runtime.spawn(async move {
            match answer.await {
                Some(Ok(_)) => {
                    tracing::info!(%room, salt, "topology namespace claimed for legacy windows");
                }
                Some(Err(error)) => {
                    tracing::warn!(%room, salt, %error, "topology namespace claim refused");
                }
                None => tracing::warn!(%room, salt, "topology namespace claim abandoned"),
            }
        });
    }

    /// Whether changes not yet admitted in `room` are due to go again.
    fn due_to_send(&self, room: RoomId, now: Instant) -> bool {
        let Ok(mut rooms) = self.hosted_topology.rooms.lock() else {
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
    ) -> bool {
        if !self.due_to_send(room, now) {
            return false;
        }
        match self.runtime.as_ref() {
            Some(runtime) => {
                runtime.spawn_blocking(move || admit_own_topology(&owner, room, changes));
            }
            None => admit_own_topology(&owner, room, changes),
        }
        true
    }

    fn send_topology(&self, room: RoomId, changes: Vec<Vec<u8>>, now: Instant) {
        if !self.due_to_send(room, now) {
            return;
        }
        tracing::debug!(%room, changes = changes.len(), "publishing this session's windows to the room");
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

/// This session's salt, and the windows it keeps in `room`'s document: when
/// it governs the room, every window it owns; else its own legacy tabs
/// ([`own_legacy_windows`]).
fn windows_to_publish(
    tree: &crate::shell::state::WorkspaceTree,
    room: RoomId,
    governed_here: bool,
) -> (u16, Vec<Strip>) {
    let salt = tree.local_salt();
    let windows = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| {
            if governed_here {
                workspace.strips.clone()
            } else {
                own_legacy_windows(&workspace.strips, salt)
            }
        })
        .unwrap_or_default();
    (salt, windows)
}

enum Plan {
    Claim,
    Publish(Vec<Vec<u8>>),
}

#[cfg(test)]
thread_local! {
    /// Room documents [`plan`] copied to project into, on this thread.
    static PROJECTIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// What one look at a room is about: `device`'s own legacy tabs `windows`,
/// with ids minted in `salt`.
#[derive(Clone, Copy)]
struct Look<'a> {
    device: [u8; 32],
    windows: &'a [Strip],
    salt: u16,
}

/// What keeping `admitted` showing `look.windows` takes from `look.device`:
/// claiming the namespace, publishing changes, or nothing. `settled` is what
/// the last look found nothing to do for; while `admitted`, the windows, the
/// device and the salt are still that, nothing is done again without copying
/// `admitted`, and a look that finds nothing to do records itself there.
///
/// The changes are the projection onto a copy of `admitted` as `device`,
/// byte for byte what the governor admits; only how often it runs changes.
/// `pending` holds what was published before and is not admitted yet: the
/// projection builds on it, and it is published again with whatever the
/// projection adds. Projecting onto `admitted` alone would number a new
/// change as the unadmitted one before it, and two different changes with
/// one actor and sequence number poison the room document for everyone who
/// receives both ("duplicate seq 1 found for actor").
fn plan(
    admitted: &automerge::AutoCommit,
    look: Look<'_>,
    settled: &mut Option<Settled>,
    pending: &mut Vec<Vec<u8>>,
) -> Result<Option<Plan>, String> {
    use automerge::transaction::Transactable as _;
    use tilo_automerge::ApplyIncoming as _;
    let Look {
        device,
        windows,
        salt,
    } = look;
    let looked_at = Settled {
        heads: admitted.base_heads(),
        device,
        salt,
        windows: serde_json::to_vec(windows).map_err(|error| error.to_string())?,
    };
    if pending.is_empty() && settled.as_ref() == Some(&looked_at) {
        return Ok(None);
    }
    *settled = None;
    let registered = tilo_desktop_model::projection::contents_owners_of(admitted);
    match registered.get(&salt) {
        Some(key) if key == &hex::encode(device) => {}
        Some(_) => {
            pending.clear();
            return Err("this session's topology namespace belongs to another device".into());
        }
        None if windows.is_empty() && pending.is_empty() => {
            *settled = Some(looked_at);
            return Ok(None);
        }
        None => return Ok(Some(Plan::Claim)),
    }
    #[cfg(test)]
    PROJECTIONS.with(|count| count.set(count.get().saturating_add(1)));
    let mut doc = admitted.clone();
    // What the governor admitted since is done; the rest still stands, in
    // the order it was made.
    let mut unadmitted = Vec::with_capacity(pending.len());
    for bytes in pending.drain(..) {
        let change = automerge::Change::from_bytes(bytes.clone()).map_err(|e| e.to_string())?;
        if doc.get_change_by_hash(&change.hash()).is_none() {
            unadmitted.push((bytes, change));
        }
    }
    doc.apply_incoming(unadmitted.iter().map(|(_, change)| change.clone()))
        .map_err(|error| error.to_string())?;
    pending.extend(unadmitted.into_iter().map(|(bytes, _)| bytes));
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(device.to_vec()));
    tilo_desktop_model::projection::project_owned_contents(
        &mut doc,
        windows,
        salt,
        None,
        tilo_desktop_model::projection::EmptyStrips::Skip,
    )
    .map_err(|error| error.to_string())?;
    doc.commit();
    pending.extend(
        doc.get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec()),
    );
    if pending.is_empty() {
        // The room already shows these windows.
        *settled = Some(looked_at);
        return Ok(None);
    }
    // Not admitted yet: planned again on every look, which is how it is
    // resent.
    Ok(Some(Plan::Publish(pending.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Tab, WindowGroup, WindowWidth};

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
            salt,
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
        network.publish_hosted_legacy_windows(room, &tree);
        let tabs: Vec<u64> = owner
            .with_current(room, |view| {
                let contents = tilo_desktop_model::projection::contents_of(
                    view.peer().admitted_room().ok_or("no room")?,
                );
                Ok(contents
                    .iter()
                    .flat_map(|strip| &strip.groups)
                    .flat_map(|group| &group.tabs)
                    .map(|tab| tab.id)
                    .collect())
            })
            .unwrap();
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
        network.publish_hosted_legacy_windows(room, &tree);
        let tabs: Vec<u64> = owner
            .with_current(room, |view| {
                Ok(tilo_desktop_model::projection::contents_of(
                    view.peer().admitted_room().ok_or("no room")?,
                )
                .iter()
                .flat_map(|strip| &strip.groups)
                .flat_map(|group| &group.tabs)
                .map(|tab| tab.id)
                .collect())
            })
            .unwrap();
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

    /// A retained request for this device's window is applied once, claimed
    /// in this device's own namespace, and never applied again: not on the
    /// next look, and not after a restart that reads the room back.
    #[test]
    fn a_retained_layout_request_is_applied_and_claimed_exactly_once() {
        use crate::shell::state::{StripId, WorkspaceTree};
        use automerge::ReadDoc as _;
        use ed25519_dalek::SigningKey;
        use tilo_desktop_model::commands::{
            BoundCommand, CommandSource, DesktopCommand, TabLocation,
        };

        let directory = tempfile::tempdir().unwrap();
        let salt = 0x0042;
        let own = |counter: u64| (u64::from(salt) << 48) | counter;
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.set_peer_salt(salt);
        tree.persist_path = Some(directory.path().join("state.json"));
        let workspace = tree.desktop.active;
        let placeholder = |label: &str| TileContent::AppPlaceholder {
            kind: crate::shell::state::AppKind::Other("so.tilo.notes".into()),
            label: label.into(),
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
                    tabs: vec![
                        tab(own(3), placeholder("First")),
                        tab(own(4), placeholder("Second")),
                    ],
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
            salt,
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
        let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
        network.publish_hosted_legacy_windows(room, &tree);

        let request = |doc: &mut automerge::AutoCommit| {
            let bound = BoundCommand {
                source: CommandSource {
                    room: room.to_string(),
                    heads: doc.get_heads().into_iter().map(|head| head.0).collect(),
                },
                command: DesktopCommand::ReorderTab {
                    location: TabLocation {
                        strip: own(1),
                        group: own(2),
                        tab: own(4),
                    },
                    before: Some(own(3)),
                },
            };
            tilo_desktop_model::projection::LayoutRequest {
                id: "retained-reorder".into(),
                from: hex::encode(device.user_public_key),
                to_salt: salt,
                at: 1,
                command: serde_json::to_vec(&bound).unwrap(),
            }
        };
        let mut doc = owner
            .with_current(room, |view| {
                Ok(view.peer().admitted_room().unwrap().clone())
            })
            .unwrap();
        let heads = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(device.device_public_key.to_vec()));
        let retained = request(&mut doc);
        tilo_desktop_model::room_layout::request(&mut doc, &retained).unwrap();
        doc.commit();
        let changes = doc
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
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

        let order = |tree: &crate::shell::state::WorkspaceTreeHandle| {
            tree.read()
                .desktop
                .workspaces
                .iter()
                .find(|ws| ws.room() == room)
                .unwrap()
                .strips[0]
                .window_groups[0]
                .tabs
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(order(&tree), [own(3), own(4)]);

        network.apply_retained_layout_requests(
            &owner,
            room,
            &tree,
            true,
            std::time::Instant::now(),
        );
        assert_eq!(order(&tree), [own(4), own(3)], "the request moved the tab");
        let applied = owner
            .with_current(room, |view| {
                let doc = view.peer().admitted_room().unwrap();
                Ok(tilo_desktop_model::room_layout::pending(doc, salt).len())
            })
            .unwrap();
        assert_eq!(applied, 0, "the room no longer holds it pending");
        let marks = owner
            .with_current(room, |view| {
                Ok(view
                    .peer()
                    .admitted_room()
                    .unwrap()
                    .get(automerge::ROOT, "layout_applied_v1")
                    .unwrap()
                    .is_some())
            })
            .unwrap();
        assert!(marks, "the owner recorded its claim in the room");

        // Applying the same look again must not move it a second time (the
        // mark excludes it), and a restarted owner that reads the room back
        // must not either.
        network.apply_retained_layout_requests(
            &owner,
            room,
            &tree,
            true,
            std::time::Instant::now(),
        );
        assert_eq!(order(&tree), [own(4), own(3)]);
        drop(network);
        drop(owner);
        let reopened = crate::shell::native_workspace_owner::open(tree.clone()).unwrap();
        let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
        network.apply_retained_layout_requests(
            &reopened,
            room,
            &tree,
            true,
            std::time::Instant::now(),
        );
        assert_eq!(order(&tree), [own(4), own(3)], "a restart applies it once");
    }

    #[test]
    fn only_this_sessions_own_legacy_tabs_are_published() {
        let salt = 0x0042;
        let own = |counter: u64| (u64::from(salt) << 48) | counter;
        let host = |counter: u64| (0x0007_u64 << 48) | counter;
        let foreign = |surface| TileContent::Foreign {
            app_id: "org.chromium.Chromium".into(),
            surface_id: surface,
        };
        let placeholder = TileContent::AppPlaceholder {
            kind: crate::shell::state::AppKind::Other("so.tilo.notes".into()),
            label: "Notes".into(),
        };
        let strips = vec![Strip {
            id: crate::shell::state::StripId(host(1)),
            name: "Workspace".into(),
            window_groups: vec![
                WindowGroup {
                    id: host(2),
                    tabs: vec![tab(host(3), placeholder.clone())],
                    active_tab: host(3),
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                },
                WindowGroup {
                    id: own(1),
                    tabs: vec![tab(own(2), placeholder), tab(own(3), foreign(7))],
                    active_tab: own(2),
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                },
                WindowGroup {
                    id: own(4),
                    tabs: vec![tab(host(9), foreign(8))],
                    active_tab: host(9),
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                },
            ],
        }];
        let published = own_legacy_windows(&strips, salt);
        assert_eq!(published.len(), 1);
        let groups = &published[0].window_groups;
        assert_eq!(groups.len(), 1, "groups without own legacy tabs stay out");
        assert_eq!(groups[0].id, own(1));
        assert_eq!(
            groups[0].tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            [own(3)]
        );
        assert_eq!(groups[0].active_tab, own(3));
        assert!(own_legacy_windows(&strips, 0x0099).is_empty());
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
    fn unchanged_legacy_windows_do_not_copy_the_room() {
        use automerge::transaction::Transactable as _;
        let salt = 0x0042;
        let device = [0x5a; 32];
        let own = |counter: u64| (u64::from(salt) << 48) | counter;
        let windows = |surface: u64| {
            own_legacy_windows(
                &[Strip {
                    id: crate::shell::state::StripId(own(1)),
                    name: "Workspace".into(),
                    window_groups: vec![WindowGroup {
                        id: own(2),
                        tabs: vec![tab(
                            own(3),
                            TileContent::Foreign {
                                app_id: "org.gimp.GIMP".into(),
                                surface_id: surface,
                            },
                        )],
                        active_tab: own(3),
                        width: WindowWidth::Normal,
                        maximized: false,
                        focus_history: Vec::new(),
                    }],
                }],
                salt,
            )
        };
        let projections = || PROJECTIONS.with(std::cell::Cell::get);
        let mut room = automerge::AutoCommit::new();
        let owners = room
            .put_object(
                automerge::ROOT,
                tilo_desktop_model::projection::ROOM_CONTENTS_OWNERS_KEY,
                automerge::ObjType::Map,
            )
            .expect("a map");
        room.put(&owners, format!("{salt:x}"), hex::encode(device))
            .expect("an owner");
        room.commit();
        let mut settled = None;
        let mut pending = Vec::new();

        let first = windows(7);
        let Ok(Some(Plan::Publish(changes))) = plan(
            &room,
            Look {
                device,
                windows: &first,
                salt,
            },
            &mut settled,
            &mut pending,
        ) else {
            panic!("a window the room does not show yet is published");
        };
        assert!(
            matches!(
                plan(
                    &room,
                    Look {
                        device,
                        windows: &first,
                        salt
                    },
                    &mut settled,
                    &mut pending
                ),
                Ok(Some(Plan::Publish(_)))
            ),
            "until the governor admits it, it is planned again so it is resent",
        );
        assert_eq!(projections(), 2);

        // Phase `an_admitted_room_is_looked_at_once`. The governor admits
        // the changes: one more look finds nothing, and every look after
        // it copies nothing while neither side moved.
        for change in &changes {
            tilo_automerge::ApplyIncoming::load_incoming(&mut room, change)
                .expect("the governor admits them");
        }
        assert!(matches!(
            plan(
                &room,
                Look {
                    device,
                    windows: &first,
                    salt
                },
                &mut settled,
                &mut pending
            ),
            Ok(None)
        ));
        assert_eq!(projections(), 3);
        for _ in 0..100 {
            assert!(matches!(
                plan(
                    &room,
                    Look {
                        device,
                        windows: &windows(7),
                        salt
                    },
                    &mut settled,
                    &mut pending
                ),
                Ok(None)
            ));
        }
        assert_eq!(projections(), 3, "a settled room is not copied again");

        assert!(matches!(
            plan(
                &room,
                Look {
                    device,
                    windows: &windows(8),
                    salt
                },
                &mut settled,
                &mut pending
            ),
            Ok(Some(Plan::Publish(_)))
        ));
        assert_eq!(projections(), 4, "a changed window is projected");
        // Changed back before the governor took the change: the change
        // already sent stands, so changing back is a change after it, and
        // the room ends up showing the window as it is now.
        let Ok(Some(Plan::Publish(back))) = plan(
            &room,
            Look {
                device,
                windows: &first,
                salt,
            },
            &mut settled,
            &mut pending,
        ) else {
            panic!("changing back after an unadmitted change is a change of its own");
        };
        assert_eq!(projections(), 5);
        for change in &back {
            tilo_automerge::ApplyIncoming::load_incoming(&mut room, change)
                .expect("the governor admits them in order");
        }
        assert!(matches!(
            plan(
                &room,
                Look {
                    device,
                    windows: &first,
                    salt
                },
                &mut settled,
                &mut pending
            ),
            Ok(None)
        ));
        assert_eq!(projections(), 6);
        room.put(automerge::ROOT, "unrelated", 1_u64)
            .expect("another write");
        room.commit();
        assert!(matches!(
            plan(
                &room,
                Look {
                    device,
                    windows: &first,
                    salt
                },
                &mut settled,
                &mut pending
            ),
            Ok(None)
        ));
        assert_eq!(projections(), 7, "a moved room document is looked at again");
    }

    /// A room in use carries thousands of edits of history, and copying it
    /// to project into on every look is what a settled room must not cost.
    /// Counted, not timed: without the memo of the last look every look
    /// copies the room, and with it no look does while nothing moved.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
    fn a_settled_room_is_looked_at_without_copying_it() {
        use automerge::transaction::Transactable as _;
        const LOOKS: u64 = 50;
        let salt = 0x0042;
        let device = [0x5a; 32];
        let own = |counter: u64| (u64::from(salt) << 48) | counter;
        let copies = || PROJECTIONS.with(std::cell::Cell::get);
        let mut room = automerge::AutoCommit::new();
        let owners = room
            .put_object(
                automerge::ROOT,
                tilo_desktop_model::projection::ROOM_CONTENTS_OWNERS_KEY,
                automerge::ObjType::Map,
            )
            .expect("a map");
        room.put(&owners, format!("{salt:x}"), hex::encode(device))
            .expect("an owner");
        // A room that has been in use: a few thousand edits of history.
        let notes = room
            .put_object(automerge::ROOT, "history", automerge::ObjType::Map)
            .expect("a map");
        for edit in 0..4_000_u64 {
            room.put(&notes, format!("k{}", edit % 64), edit)
                .expect("an edit");
            if edit % 8 == 0 {
                room.commit();
            }
        }
        room.commit();
        let windows = own_legacy_windows(
            &[Strip {
                id: crate::shell::state::StripId(own(1)),
                name: "Workspace".into(),
                window_groups: vec![WindowGroup {
                    id: own(2),
                    tabs: vec![tab(
                        own(3),
                        TileContent::Foreign {
                            app_id: "org.gimp.GIMP".into(),
                            surface_id: 7,
                        },
                    )],
                    active_tab: own(3),
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                }],
            }],
            salt,
        );
        let mut settled = None;
        let mut pending = Vec::new();
        let Ok(Some(Plan::Publish(changes))) = plan(
            &room,
            Look {
                device,
                windows: &windows,
                salt,
            },
            &mut settled,
            &mut pending,
        ) else {
            panic!("the window is published once");
        };
        for change in &changes {
            tilo_automerge::ApplyIncoming::load_incoming(&mut room, change).expect("admitted");
        }

        assert!(
            room.get_changes(&[]).len() > 500,
            "the room carries a history"
        );

        let before = copies();
        for _ in 0..LOOKS {
            let mut fresh = None;
            assert!(matches!(
                plan(
                    &room,
                    Look {
                        device,
                        windows: &windows,
                        salt
                    },
                    &mut fresh,
                    &mut pending
                ),
                Ok(None)
            ));
        }
        assert_eq!(
            copies().saturating_sub(before),
            LOOKS,
            "every unmemoised look copies"
        );

        // One look records that the admitted room needs nothing; no look
        // after it copies the room while neither side moved.
        assert!(matches!(
            plan(
                &room,
                Look {
                    device,
                    windows: &windows,
                    salt
                },
                &mut settled,
                &mut pending
            ),
            Ok(None)
        ));
        let before = copies();
        for _ in 0..LOOKS {
            assert!(matches!(
                plan(
                    &room,
                    Look {
                        device,
                        windows: &windows,
                        salt
                    },
                    &mut settled,
                    &mut pending
                ),
                Ok(None)
            ));
        }
        assert_eq!(copies(), before, "a settled room is not copied");
    }

    /// A window opened while the last one is still waiting for the
    /// governor builds on it: the resend is the same change, the new window
    /// is the next one by the same device, and the governor (and every
    /// member after it) takes both. Projecting onto the admitted room alone
    /// numbered both as the device's first change, which nobody can take
    /// once they hold one of them.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
    fn a_window_opened_before_the_last_is_admitted_builds_on_it() {
        use automerge::transaction::Transactable as _;
        let salt = 0x0042;
        let device = [0x5a; 32];
        let own = |counter: u64| (u64::from(salt) << 48) | counter;
        let group = |id: u64, tab_id: u64, surface: u64| WindowGroup {
            id: own(id),
            tabs: vec![tab(
                own(tab_id),
                TileContent::Foreign {
                    app_id: "so.tilo.chat".into(),
                    surface_id: surface,
                },
            )],
            active_tab: own(tab_id),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        };
        let strip = |groups: Vec<WindowGroup>| {
            own_legacy_windows(
                &[Strip {
                    id: crate::shell::state::StripId(own(1)),
                    name: "Main".into(),
                    window_groups: groups,
                }],
                salt,
            )
        };
        let mut room = automerge::AutoCommit::new();
        let owners = room
            .put_object(
                automerge::ROOT,
                tilo_desktop_model::projection::ROOM_CONTENTS_OWNERS_KEY,
                automerge::ObjType::Map,
            )
            .expect("a map");
        room.put(&owners, format!("{salt:x}"), hex::encode(device))
            .expect("an owner");
        room.commit();
        let (mut settled, mut pending) = (None, Vec::new());

        let one = strip(vec![group(2, 3, 7)]);
        let Ok(Some(Plan::Publish(first))) = plan(
            &room,
            Look {
                device,
                windows: &one,
                salt,
            },
            &mut settled,
            &mut pending,
        ) else {
            panic!("the first window is published");
        };
        let Ok(Some(Plan::Publish(again))) = plan(
            &room,
            Look {
                device,
                windows: &one,
                salt,
            },
            &mut settled,
            &mut pending,
        ) else {
            panic!("an unadmitted window is published again");
        };
        assert_eq!(again, first, "the resend is the very same change");

        let two = strip(vec![group(2, 3, 7), group(4, 5, 8)]);
        let Ok(Some(Plan::Publish(both))) = plan(
            &room,
            Look {
                device,
                windows: &two,
                salt,
            },
            &mut settled,
            &mut pending,
        ) else {
            panic!("the second window is published");
        };
        assert_eq!(
            both[..first.len()],
            first[..],
            "the first change still leads"
        );
        assert!(
            both.len() > first.len(),
            "the second window is a change of its own"
        );
        let seqs: Vec<u64> = both
            .iter()
            .map(|bytes| {
                automerge::Change::from_bytes(bytes.clone())
                    .expect("a change")
                    .seq()
            })
            .collect();
        let mut expected = seqs.clone();
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(
            seqs, expected,
            "each change has a sequence number of its own: {seqs:?}"
        );

        // The governor takes them all, and the room then needs nothing.
        for change in &both {
            tilo_automerge::ApplyIncoming::load_incoming(&mut room, change)
                .expect("the governor admits every change");
        }
        assert!(matches!(
            plan(
                &room,
                Look {
                    device,
                    windows: &two,
                    salt,
                },
                &mut settled,
                &mut pending,
            ),
            Ok(None)
        ));
        assert!(pending.is_empty(), "nothing is waiting once admitted");
    }
}
