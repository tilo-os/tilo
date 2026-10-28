//! Which windows of another member a device views in a room, and which
//! members view this device's.
//!
//! A host or unconfined program's window (a terminal, any Wayland or X
//! client) crosses a room that shares its layout as a tab hosted on the
//! device running it. Each member's device that shows such a tab tells that
//! host which of its windows it shows, and at what size
//! ([`NeighbourMsg::SurfaceViewRequest`]), to the host alone over the
//! authenticated direct route, and repeats it while it shows them. The host
//! streams each named window to that member, read only, while the list keeps
//! arriving and names it: encoded only while someone views it, sent to the
//! viewer alone over the direct route (end-to-end encrypted to its device).
//! The person's own devices use their personal fabric for the same
//! ([`crate::shell::device_fabric_runtime`]).
//!
//! The room's gate admitted the request before it reaches here: its sender
//! may read the room it names. The compositor streams only windows of that
//! room, and none of a room whose layout is personal.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use neighbour_wire::{
    MAX_SURFACE_VIEW_DECODERS, MAX_SURFACE_VIEW_SIDE, MAX_SURFACE_VIEWS, SurfaceDecodeLimit,
    SurfaceView,
};
use tilo_lock::MutexRecover as _;

use super::{NetCmd, ShellNetwork, room_topic_id};
use crate::network::actor::WorkspaceControlRoute;
use crate::network::protocol::NeighbourMsg;
use crate::shell::state::RoomId;

/// How often an unchanged, non-empty list is repeated.
const REFRESH: Duration = Duration::from_secs(2);
/// A viewer whose list has not been repeated for this long has stopped
/// viewing: it closed the window, slept, or lost the network.
const EXPIRY: Duration = Duration::from_secs(6);

/// One member's device viewing a window this device hosts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomWindowViewer {
    /// The viewer's peer handle, the address its stream travels under.
    pub peer: u64,
    pub room: RoomId,
    pub width: u32,
    pub height: u32,
    /// The largest picture the viewer's hardware decoders take, by codec.
    pub decode_limits: BTreeMap<media::StreamCodec, (u32, u32)>,
}

struct Viewing {
    views: Vec<SurfaceView>,
    decode_limits: BTreeMap<media::StreamCodec, (u32, u32)>,
    at: Instant,
}

struct Told {
    views: Vec<SurfaceView>,
    at: Instant,
}

#[derive(Default)]
pub(super) struct State {
    /// What each member's device views, by its peer handle and the room.
    inbound: std::sync::Mutex<HashMap<(u64, RoomId), Viewing>>,
    /// What each host was last told, by its device and the room.
    outbound: std::sync::Mutex<HashMap<(iroh::EndpointId, RoomId), Told>>,
}

fn within_bounds(views: &[SurfaceView], decode: &[SurfaceDecodeLimit]) -> bool {
    let side = |value: u32| (1..=MAX_SURFACE_VIEW_SIDE).contains(&value);
    views.len() <= MAX_SURFACE_VIEWS
        && decode.len() <= MAX_SURFACE_VIEW_DECODERS
        && views
            .iter()
            .all(|view| side(view.width) && side(view.height))
}

/// The limits a viewer reported, by codec; codecs this build does not know
/// and codecs without a limit carry none.
fn limits_of(decode: &[SurfaceDecodeLimit]) -> BTreeMap<media::StreamCodec, (u32, u32)> {
    decode
        .iter()
        .filter_map(|entry| {
            Some((
                media::StreamCodec::from_wire_label(&entry.codec)?,
                entry.max?,
            ))
        })
        .collect()
}

/// What this device's hardware decoders take.
fn local_decode() -> Vec<SurfaceDecodeLimit> {
    let caps = media::realtime::local_capabilities();
    caps.hardware_decode
        .iter()
        .take(MAX_SURFACE_VIEW_DECODERS)
        .map(|codec| SurfaceDecodeLimit {
            codec: codec.wire_label().to_owned(),
            max: caps.hardware_decode_max.get(codec).copied(),
        })
        .collect()
}

impl State {
    fn record(
        &self,
        (peer, room): (u64, RoomId),
        views: Vec<SurfaceView>,
        decode: &[SurfaceDecodeLimit],
        now: Instant,
    ) {
        if views.is_empty() {
            self.inbound.lock_recover().remove(&(peer, room));
        } else {
            let viewing = Viewing {
                views,
                decode_limits: limits_of(decode),
                at: now,
            };
            self.inbound.lock_recover().insert((peer, room), viewing);
        }
    }

    fn viewers(&self, now: Instant) -> BTreeMap<u64, Vec<RoomWindowViewer>> {
        let mut viewers: BTreeMap<u64, Vec<RoomWindowViewer>> = BTreeMap::new();
        let mut inbound = self.inbound.lock_recover();
        inbound.retain(|_, viewing| now.saturating_duration_since(viewing.at) < EXPIRY);
        for ((peer, room), viewing) in inbound.iter() {
            for view in &viewing.views {
                viewers.entry(view.tab).or_default().push(RoomWindowViewer {
                    peer: *peer,
                    room: *room,
                    width: view.width,
                    height: view.height,
                    decode_limits: viewing.decode_limits.clone(),
                });
            }
        }
        drop(inbound);
        for list in viewers.values_mut() {
            list.sort_by_key(|viewer| (viewer.peer, viewer.room.to_string()));
        }
        viewers
    }

    /// The lists to send now, by host and room: on a change of what is shown
    /// of its windows, and as a refresh while any is. A host none of whose
    /// windows is shown any more hears the empty list once, so its streams
    /// end at once rather than at expiry.
    fn due(
        &self,
        shown: BTreeMap<(iroh::EndpointId, RoomId), Vec<SurfaceView>>,
        now: Instant,
    ) -> Vec<((iroh::EndpointId, RoomId), Vec<SurfaceView>)> {
        let mut outbound = self.outbound.lock_recover();
        let mut due = Vec::new();
        let gone: Vec<_> = outbound
            .keys()
            .filter(|key| !shown.contains_key(key))
            .copied()
            .collect();
        for key in gone {
            outbound.remove(&key);
            due.push((key, Vec::new()));
        }
        for (key, views) in shown {
            let fresh = outbound.get(&key).is_some_and(|told| {
                told.views == views && now.saturating_duration_since(told.at) < REFRESH
            });
            if !fresh {
                outbound.insert(
                    key,
                    Told {
                        views: views.clone(),
                        at: now,
                    },
                );
                due.push((key, views));
            }
        }
        drop(outbound);
        due
    }
}

impl ShellNetwork {
    /// Take what member `peer` (its peer handle, as the transport
    /// authenticated it) says it views of this device's windows in `room`,
    /// arrived on workspace topic `ws`. A request naming another room than
    /// its topic's, or past the wire's bounds, is dropped.
    pub(super) fn note_surface_views(
        &self,
        ws: u64,
        peer: u64,
        room: &str,
        views: Vec<SurfaceView>,
        decode: &[SurfaceDecodeLimit],
    ) {
        let Some(room) = RoomId::parse(room).filter(|room| room_topic_id(*room) == ws) else {
            tracing::warn!(ws, peer, "window views for another room than their topic's");
            return;
        };
        if peer == self.local_peer_hash || !within_bounds(&views, decode) {
            return;
        }
        self.room_window_views
            .record((peer, room), views, decode, Instant::now());
    }

    /// The members viewing each of this device's windows, by tab: those
    /// that asked directly, and each room's carrier for those that view it
    /// through the room's relay.
    pub fn room_window_viewers(&self) -> BTreeMap<u64, Vec<RoomWindowViewer>> {
        let mut viewers = self.room_window_views.viewers(Instant::now());
        for (tab, carried) in self.room_carrier_viewers() {
            viewers.entry(tab).or_default().extend(carried);
        }
        viewers
    }

    /// Tell each host which of its windows this device shows as a member of
    /// their room, given as (room, host device key, window); cheap when
    /// nothing changed, so call it every tick. The person's own devices are
    /// told over their personal fabric instead, and are left out here. A
    /// host this device reaches only through a relay is not told (a relay
    /// would fan the list out to the room): its windows are shown through
    /// the room's carrier instead (`super::room_window_carriers`).
    pub fn publish_room_window_views(&self, shown: Vec<(RoomId, String, String, SurfaceView)>) {
        let own = self.online_device_ids();
        let mut by_host: BTreeMap<(iroh::EndpointId, RoomId), Vec<SurfaceView>> = BTreeMap::new();
        let mut apps: HashMap<(iroh::EndpointId, RoomId, u64), String> = HashMap::new();
        for (room, host, app_id, view) in shown {
            let Ok(device) = crate::shell::personal_continuity::DeviceId::new(host.clone()) else {
                continue;
            };
            if own.contains(&device) {
                continue;
            }
            let Some(target) = hex::decode(&host)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                .and_then(|bytes| iroh::EndpointId::from_bytes(&bytes).ok())
            else {
                continue;
            };
            if target == self.local_peer_id {
                continue;
            }
            apps.insert((target, room, view.tab), app_id);
            by_host.entry((target, room)).or_default().push(view);
        }
        for views in by_host.values_mut() {
            views.sort_by_key(|view| view.tab);
            views.dedup_by_key(|view| view.tab);
            views.truncate(MAX_SURFACE_VIEWS);
        }
        let direct = |target: iroh::EndpointId, room: RoomId| {
            self.workspace_control_direct
                .lock()
                .is_ok_and(|peers| peers.contains(&(room_topic_id(room), target)))
        };
        let carried = by_host
            .iter()
            .filter(|((target, room), _)| !direct(*target, *room))
            .flat_map(|((target, room), views)| {
                views.iter().filter_map(|view| {
                    let app_id = apps.get(&(*target, *room, view.tab))?.clone();
                    Some((*room, *target.as_bytes(), view.tab, app_id))
                })
            })
            .collect();
        self.show_room_windows_through_carriers(carried);
        let due = self.room_window_views.due(by_host, Instant::now());
        if due.is_empty() {
            return;
        }
        let decode = local_decode();
        for ((target, room), views) in due {
            let ws = room_topic_id(room);
            if !direct(target, room) {
                tracing::debug!(%room, host = %target, "no direct route to the window's host");
                continue;
            }
            let msg = NeighbourMsg::SurfaceViewRequest {
                peer: self.local_peer_hash,
                room: room.to_string(),
                views,
                decode: decode.clone(),
            };
            let _ = self.cmd_tx.try_send(NetCmd::PublishWorkspaceControl {
                ws,
                msg,
                route: WorkspaceControlRoute::Direct { target },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(tab: u64) -> SurfaceView {
        SurfaceView {
            tab,
            width: 1280,
            height: 720,
        }
    }

    /// A member's views are kept while repeated, end with an empty list or
    /// once they stop arriving, and name the room they were made in.
    #[test]
    fn a_members_views_last_while_repeated_and_end_with_an_empty_list() {
        let state = State::default();
        let room = RoomId::new();
        let t0 = Instant::now();
        let h264 = [SurfaceDecodeLimit {
            codec: "h264".into(),
            max: Some((1920, 1080)),
        }];
        state.record((7, room), vec![view(42)], &h264, t0);
        let viewers = state.viewers(t0);
        assert_eq!(
            viewers.get(&42).map(Vec::as_slice),
            Some(
                [RoomWindowViewer {
                    peer: 7,
                    room,
                    width: 1280,
                    height: 720,
                    decode_limits: [(media::StreamCodec::H264, (1920, 1080))].into(),
                }]
                .as_slice()
            )
        );
        assert!(state.viewers(t0 + EXPIRY).is_empty(), "stopped arriving");
        state.record((7, room), vec![view(42)], &h264, t0);
        state.record((7, room), Vec::new(), &h264, t0);
        assert!(state.viewers(t0).is_empty(), "said it stopped");
    }

    #[test]
    fn a_host_hears_its_list_on_a_change_as_a_refresh_and_once_empty() {
        let state = State::default();
        let room = RoomId::new();
        let host = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let t0 = Instant::now();
        let shown = BTreeMap::from([((host, room), vec![view(1)])]);
        assert_eq!(state.due(shown.clone(), t0).len(), 1);
        assert!(state.due(shown.clone(), t0).is_empty(), "unchanged");
        assert_eq!(state.due(shown, t0 + REFRESH).len(), 1, "refreshed");
        let gone = state.due(BTreeMap::new(), t0 + REFRESH);
        assert_eq!(gone, [((host, room), Vec::new())]);
        assert!(state.due(BTreeMap::new(), t0 + REFRESH).is_empty());
    }

    #[test]
    fn views_past_the_wires_bounds_are_refused() {
        assert!(within_bounds(&[view(1)], &[]));
        let huge = SurfaceView {
            tab: 1,
            width: MAX_SURFACE_VIEW_SIDE + 1,
            height: 10,
        };
        assert!(!within_bounds(&[huge], &[]));
        let many = (0..=MAX_SURFACE_VIEWS as u64).map(view).collect::<Vec<_>>();
        assert!(!within_bounds(&many, &[]));
    }
}
