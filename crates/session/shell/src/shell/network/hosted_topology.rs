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
use std::collections::HashMap;
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
    /// window of the room's workspace while the session is watched.
    pub fn viewer_hosts(&self, room: RoomId) -> std::collections::HashSet<u64> {
        let mut hosts = self.workspace_hosts(room_topic_id(room));
        if let Ok(owner) = self.native_owner()
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

    /// Keep `room`'s document showing this session's legacy windows among
    /// its workspace's layout in `tree`, when another device governs `room`.
    /// Only such a room's layout is read, and only this session's own legacy
    /// tabs are copied out of it ([`own_legacy_windows`]). Cheap when nothing
    /// changed: while neither those windows nor the admitted room document
    /// moved since a look found nothing to publish, the document is not
    /// copied. Call every tick.
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
        if !self.governed_elsewhere(room) {
            return;
        }
        let (salt, windows) = {
            let tree = tree.read();
            let salt = tree.local_salt();
            let windows = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .map(|workspace| own_legacy_windows(&workspace.strips, salt))
                .unwrap_or_default();
            drop(tree);
            (salt, windows)
        };
        if salt == 0 {
            return;
        }
        let Ok(owner) = self.native_owner() else {
            return;
        };
        let mut settled = self
            .hosted_topology
            .rooms
            .lock()
            .ok()
            .and_then(|mut rooms| rooms.get_mut(&room)?.settled.take());
        let planned = owner.with_current(room, |view| {
            let admitted = view
                .peer()
                .admitted_room()
                .ok_or("room governance missing")?;
            plan(
                admitted,
                view.device().device_public_key,
                &windows,
                salt,
                &mut settled,
            )
        });
        if let Ok(mut rooms) = self.hosted_topology.rooms.lock() {
            rooms.entry(room).or_default().settled = settled;
        }
        match planned {
            Ok(Some(Plan::Claim)) => self.claim_namespace(room, salt, now),
            Ok(Some(Plan::Publish(changes))) => self.send_topology(room, changes, now),
            Ok(None) => {}
            Err(error) => tracing::debug!(%room, %error, "hosted legacy windows not published"),
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

    fn send_topology(&self, room: RoomId, changes: Vec<Vec<u8>>, now: Instant) {
        {
            let Ok(mut rooms) = self.hosted_topology.rooms.lock() else {
                return;
            };
            let state = rooms.entry(room).or_default();
            if state
                .sent_at
                .is_some_and(|at| now.saturating_duration_since(at) < RESEND_AFTER)
            {
                return;
            }
            state.sent_at = Some(now);
        }
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

enum Plan {
    Claim,
    Publish(Vec<Vec<u8>>),
}

#[cfg(test)]
thread_local! {
    /// Room documents [`plan`] copied to project into, on this thread.
    static PROJECTIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// What keeping `admitted` showing `windows`, this session's own legacy
/// tabs with ids minted in `salt`, takes from `device`: claiming the
/// namespace, publishing changes, or nothing. `settled` is what the last
/// look found nothing to do for; while `admitted`, `windows`, `device` and
/// `salt` are still that, nothing is done again without copying `admitted`,
/// and a look that finds nothing to do records itself there.
///
/// The changes are the projection onto a copy of `admitted` as `device`,
/// byte for byte what the governor admits; only how often it runs changes.
fn plan(
    admitted: &automerge::AutoCommit,
    device: [u8; 32],
    windows: &[Strip],
    salt: u16,
    settled: &mut Option<Settled>,
) -> Result<Option<Plan>, String> {
    use automerge::transaction::Transactable as _;
    let looked_at = Settled {
        heads: admitted.base_heads(),
        device,
        salt,
        windows: serde_json::to_vec(windows).map_err(|error| error.to_string())?,
    };
    if settled.as_ref() == Some(&looked_at) {
        return Ok(None);
    }
    *settled = None;
    let registered = tilo_desktop_model::projection::contents_owners_of(admitted);
    match registered.get(&salt) {
        Some(key) if key == &hex::encode(device) => {}
        Some(_) => {
            return Err("this session's topology namespace belongs to another device".into());
        }
        None if windows.is_empty() => {
            *settled = Some(looked_at);
            return Ok(None);
        }
        None => return Ok(Some(Plan::Claim)),
    }
    #[cfg(test)]
    PROJECTIONS.with(|count| count.set(count.get().saturating_add(1)));
    let mut doc = admitted.clone();
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
    let changes = doc
        .get_changes(&heads)
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect::<Vec<_>>();
    if changes.is_empty() {
        // The room already shows these windows. Changes not yet admitted
        // are planned again on every look, which is how they are resent.
        *settled = Some(looked_at);
        return Ok(None);
    }
    Ok(Some(Plan::Publish(changes)))
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

    /// A relay forwards a browser's input (wheel, a paste, a close among
    /// it) in its wire vocabulary; each must be what this session decodes.
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
        ];
        for (session, wire) in pairs {
            assert_eq!(
                postcard::to_allocvec(&session).unwrap(),
                postcard::to_allocvec(&wire).unwrap(),
                "{session:?}"
            );
        }
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
                },
                WindowGroup {
                    id: own(1),
                    tabs: vec![tab(own(2), placeholder), tab(own(3), foreign(7))],
                    active_tab: own(2),
                    width: WindowWidth::Normal,
                    maximized: false,
                },
                WindowGroup {
                    id: own(4),
                    tabs: vec![tab(host(9), foreign(8))],
                    active_tab: host(9),
                    width: WindowWidth::Normal,
                    maximized: false,
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
                    }],
                }],
                salt,
            )
        };
        let projections = || PROJECTIONS.with(std::cell::Cell::get);
        // The governor has registered this session's namespace to this device.
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

        // Phase `windows_the_room_lacks_are_published_on_every_look`.
        let first = windows(7);
        let Ok(Some(Plan::Publish(changes))) = plan(&room, device, &first, salt, &mut settled)
        else {
            panic!("a window the room does not show yet is published");
        };
        assert!(
            matches!(
                plan(&room, device, &first, salt, &mut settled),
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
            plan(&room, device, &first, salt, &mut settled),
            Ok(None)
        ));
        assert_eq!(projections(), 3);
        for _ in 0..100 {
            assert!(matches!(
                plan(&room, device, &windows(7), salt, &mut settled),
                Ok(None)
            ));
        }
        assert_eq!(projections(), 3, "a settled room is not copied again");

        // Phase `a_window_or_room_change_is_looked_at_again`.
        assert!(matches!(
            plan(&room, device, &windows(8), salt, &mut settled),
            Ok(Some(Plan::Publish(_)))
        ));
        assert_eq!(projections(), 4, "a changed window is projected");
        assert!(matches!(
            plan(&room, device, &first, salt, &mut settled),
            Ok(None)
        ));
        assert_eq!(projections(), 5);
        room.put(automerge::ROOT, "unrelated", 1_u64)
            .expect("another write");
        room.commit();
        assert!(matches!(
            plan(&room, device, &first, salt, &mut settled),
            Ok(None)
        ));
        assert_eq!(projections(), 6, "a moved room document is looked at again");
    }

    /// What one look at a settled room cost before (copying and projecting
    /// the admitted document every time) against now, for a room document
    /// with some history. Printed for the record.
    #[test]
    fn a_settled_room_is_looked_at_without_copying_it() {
        use automerge::transaction::Transactable as _;
        const LOOKS: u32 = 50;
        let salt = 0x0042;
        let device = [0x5a; 32];
        let own = |counter: u64| (u64::from(salt) << 48) | counter;
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
                }],
            }],
            salt,
        );
        let mut settled = None;
        let Ok(Some(Plan::Publish(changes))) = plan(&room, device, &windows, salt, &mut settled)
        else {
            panic!("the window is published once");
        };
        for change in &changes {
            tilo_automerge::ApplyIncoming::load_incoming(&mut room, change).expect("admitted");
        }

        let started = Instant::now();
        for _ in 0..LOOKS {
            let mut fresh = None;
            assert!(matches!(
                plan(&room, device, &windows, salt, &mut fresh),
                Ok(None)
            ));
        }
        let copied = started.elapsed();
        assert!(matches!(
            plan(&room, device, &windows, salt, &mut settled),
            Ok(None)
        ));
        let started = Instant::now();
        for _ in 0..LOOKS {
            assert!(matches!(
                plan(&room, device, &windows, salt, &mut settled),
                Ok(None)
            ));
        }
        let skipped = started.elapsed();
        eprintln!(
            "TIL-136 hosted topology look at a settled room ({} changes): copied and projected {:?}, settled {:?}",
            room.get_changes(&[]).len(),
            copied / LOOKS,
            skipped / LOOKS,
        );
        assert!(skipped < copied, "settled {skipped:?} vs copied {copied:?}");
    }
}
