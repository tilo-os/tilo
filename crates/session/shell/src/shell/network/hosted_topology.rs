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
    /// `strips` (its workspace's layout, ids minted in `salt`), when another
    /// device governs `room`. Cheap when nothing changed; call every tick.
    pub fn publish_hosted_legacy_windows(&self, room: RoomId, strips: &[Strip], salt: u16) {
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
        if salt == 0 || !self.governed_elsewhere(room) {
            return;
        }
        let Ok(owner) = self.native_owner() else {
            return;
        };
        let windows = own_legacy_windows(strips, salt);
        let planned = owner.with_current(room, |view| {
            let admitted = view
                .peer()
                .admitted_room()
                .ok_or("room governance missing")?;
            let device = view.device().device_public_key;
            let registered = tilo_desktop_model::projection::contents_owners_of(admitted);
            match registered.get(&salt) {
                Some(key) if key == &hex::encode(device) => {}
                Some(_) => {
                    return Err(
                        "this session's topology namespace belongs to another device".into(),
                    );
                }
                None if windows.is_empty() => return Ok(None),
                None => return Ok(Some(Plan::Claim)),
            }
            let mut doc = admitted.clone();
            let heads = doc.get_heads();
            doc.set_actor(automerge::ActorId::from(device.to_vec()));
            tilo_desktop_model::projection::project_owned_contents(
                &mut doc,
                &windows,
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
            Ok((!changes.is_empty()).then_some(Plan::Publish(changes)))
        });
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
        let answer = self.invoke_governor(
            room,
            tilo_desktop_model::commands::APP_ID,
            tilo_desktop_model::commands::RESOURCE,
            tilo_desktop_model::commands::ACTION,
            Some(serde_json::json!({ "command": command })),
        );
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

    /// A device host forwards a viewer's input with the wire vocabulary and
    /// the governor's handle; both must be what this session decodes.
    #[test]
    fn viewer_input_from_a_device_host_decodes_as_remote_input() {
        use crate::network::streaming::InputEvent as Session;
        use tilo_delivery_protocol::delivery_v3::live_wire::legacy_window::InputEvent as Wire;
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
}
