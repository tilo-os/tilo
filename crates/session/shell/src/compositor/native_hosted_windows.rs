//! Which legacy windows stream to whom: this device's windows to the
//! devices viewing them, and the windows hosted on other devices that this
//! device shows, which it asks their hosts to stream it, as one of the
//! person's devices or as a member of the room.

use std::collections::HashMap;

use super::{DesiredShares, MemberViewers, PersonalViewers, RoomHosts, all_hosts};

/// The legacy windows to encode for viewers: none while nobody watches the
/// backend, so a headless machine's encoders stop and receivers get
/// `SurfaceStreamEnd` at once.
///
/// `viewer_hosts` are each room's workspace hosts when the backend is
/// headless: its viewers watch through that host (which decides which of
/// them see and control what), so every legacy window of a workspace whose
/// room has one is shared with it, not only the explicitly shared. A headless
/// machine's "active" workspace is nobody's screen, so every room counts; a
/// room's windows go only to that room's hosts.
///
/// `personal` are the person's other devices showing a tab, by tab: a legacy
/// window streams to each device showing its tab, in whichever workspace it
/// is, since those devices show their own choice of workspace.
///
/// `members` are the room members viewing a tab, by tab: a window streams to
/// each member of its room who views it, read only, while the room shares
/// its layout; a room whose layout is personal shares no window, so none
/// of its windows streams to anyone but the person's own devices.
///
/// Each share comes with the room its tab is in, whose topic carries it.
pub(super) fn desired_legacy_shares(
    tree: &crate::shell::state::WorkspaceTree,
    watched: bool,
    viewer_hosts: &RoomHosts,
    personal: &PersonalViewers,
    members: &MemberViewers,
) -> DesiredShares {
    if !watched {
        return HashMap::new();
    }
    let hosts = all_hosts(viewer_hosts);
    let mut desired = HashMap::new();
    for workspace in &tree.desktop.workspaces {
        let active = workspace.id == tree.desktop.active;
        for tab in workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
        {
            let crate::shell::state::TileContent::Foreign { app_id, surface_id } = &tab.tile else {
                continue;
            };
            let mut peers = std::collections::HashSet::new();
            if active
                && let Some(shared) = workspace
                    .shared_surfaces
                    .iter()
                    .find_map(|(surface, peers)| (surface == *surface_id).then(|| peers.clone()))
            {
                peers.extend(shared);
            }
            if let Some(room_hosts) = viewer_hosts.get(&workspace.room()) {
                peers.extend(room_hosts);
            }
            let mut decode_limits = HashMap::new();
            if let Some(viewers) = personal.get(&tab.id) {
                for viewer in viewers {
                    peers.insert(viewer.peer);
                    decode_limits.insert(viewer.peer, viewer.decode_limits.clone());
                }
            }
            let mut read_only = std::collections::HashSet::new();
            if let Some(viewers) = members.get(&tab.id)
                && !tree.room_layout(workspace.room()).is_personal()
            {
                for viewer in viewers
                    .iter()
                    .filter(|viewer| viewer.room == workspace.room())
                {
                    // Someone the window is already shared with keeps the
                    // control that share gave them.
                    if peers.insert(viewer.peer) {
                        read_only.insert(viewer.peer);
                        decode_limits.insert(viewer.peer, viewer.decode_limits.clone());
                    }
                }
            }
            if !peers.is_empty() {
                // Hosts bridge the window to web members, which draw pages
                // themselves; any other viewer decodes video.
                let paint = peers.iter().all(|peer| hosts.contains(peer));
                desired.insert(
                    *surface_id,
                    (
                        crate::compositor::state::LegacyShare {
                            app_id: app_id.clone(),
                            tab: tab.id,
                            viewers: peers,
                            read_only,
                            decode_limits,
                            paint,
                        },
                        workspace.room(),
                    ),
                );
            }
        }
    }
    desired
}

/// The rooms whose layouts are shared in which this session hosts a legacy
/// window: their carriers are kept, so members that reach this device only
/// through a room's relay view the windows there.
pub(super) fn rooms_hosting_windows(
    tree: &crate::shell::state::WorkspaceTree,
) -> std::collections::BTreeSet<tilo_desktop_model::RoomId> {
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| !tree.room_layout(workspace.room()).is_personal())
        .filter(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .any(|tab| matches!(tab.tile, crate::shell::state::TileContent::Foreign { .. }))
        })
        .map(crate::shell::state::Workspace::room)
        .collect()
}

/// Whether one of the person's other devices shows a legacy window this
/// session hosts.
pub(super) fn hosts_viewed_window(
    tree: &crate::shell::state::WorkspaceTree,
    personal: &PersonalViewers,
    members: &MemberViewers,
) -> bool {
    (!personal.is_empty() || !members.is_empty())
        && tree.desktop.workspaces.iter().any(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .any(|tab| {
                    matches!(tab.tile, crate::shell::state::TileContent::Foreign { .. })
                        && (personal.contains_key(&tab.id) || members.contains_key(&tab.id))
                })
        })
}

/// The size a hidden window is shown at elsewhere: the largest of its
/// viewers' tiles, in logical pixels, so no viewer upscales it.
pub(super) fn remote_extents(
    desired: &DesiredShares,
    personal: &PersonalViewers,
    members: &MemberViewers,
) -> HashMap<u64, (i32, i32)> {
    desired
        .iter()
        .filter_map(|(surface, (share, _))| {
            let sizes = personal
                .get(&share.tab)
                .into_iter()
                .flatten()
                .map(|viewer| (viewer.width, viewer.height))
                .chain(
                    members
                        .get(&share.tab)
                        .into_iter()
                        .flatten()
                        .filter(|viewer| share.viewers.contains(&viewer.peer))
                        .map(|viewer| (viewer.width, viewer.height)),
                );
            // A viewer that names no size (a room's carrier, whose members
            // show the window at its own) asks for none.
            let (width, height) = sizes
                .filter(|(width, height)| *width > 0 && *height > 0)
                .max_by_key(|(width, height)| {
                    u64::from(*width).saturating_mul(u64::from(*height))
                })?;
            Some((
                *surface,
                (
                    i32::try_from(width).unwrap_or(i32::MAX),
                    i32::try_from(height).unwrap_or(i32::MAX),
                ),
            ))
        })
        .collect()
}

/// The windows other devices host that `tiles` show, with the room each
/// tab is in, its host's device key, its app id and the tile's size in
/// logical pixels: what this device asks those hosts to stream it as a
/// member of the room.
pub(super) fn shown_room_hosted_windows(
    tree: &crate::shell::state::WorkspaceTree,
    tiles: &[(u64, vello::kurbo::Rect)],
) -> Vec<(
    tilo_desktop_model::RoomId,
    String,
    String,
    neighbour_wire::SurfaceView,
)> {
    let hosted = tree
        .desktop
        .workspaces
        .iter()
        .filter(|workspace| !tree.room_layout(workspace.room()).is_personal())
        .flat_map(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .map(move |tab| (workspace.room(), tab))
        })
        .filter_map(|(room, tab)| match &tab.tile {
            crate::shell::state::TileContent::HostedWindow { app_id, .. } => {
                Some((tab.id, (room, app_id.as_str())))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    shown_hosted_windows(tree, tiles)
        .into_iter()
        .filter_map(|(host, view)| {
            let (room, app_id) = hosted.get(&view.tab)?;
            Some((
                *room,
                host,
                (*app_id).to_owned(),
                neighbour_wire::SurfaceView {
                    tab: view.tab,
                    width: view.width,
                    height: view.height,
                },
            ))
        })
        .collect()
}

/// The legacy windows other devices host that `tiles` (this device's laid-out
/// tiles, by the tab each shows) show, each with its host's device key and
/// the tile's size in logical pixels.
pub(super) fn shown_hosted_windows(
    tree: &crate::shell::state::WorkspaceTree,
    tiles: &[(u64, vello::kurbo::Rect)],
) -> Vec<(String, crate::shell::device_fabric::WindowView)> {
    let hosted = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter_map(|tab| match &tab.tile {
            crate::shell::state::TileContent::HostedWindow { host, .. } => {
                Some((tab.id, host.as_str()))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to the wire's side range and rounded first"
    )]
    let side = |value: f64| {
        value.round().clamp(
            1.0,
            f64::from(crate::shell::device_fabric::MAX_WINDOW_VIEW_SIDE),
        ) as u32
    };
    tiles
        .iter()
        .filter(|(_, rect)| rect.width() >= 1.0 && rect.height() >= 1.0)
        .filter_map(|(tab, rect)| {
            let host = hosted.get(tab)?;
            Some((
                (*host).to_owned(),
                crate::shell::device_fabric::WindowView {
                    tab: *tab,
                    width: side(rect.width()),
                    height: side(rect.height()),
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::{MemberViewers, RoomHosts};
    use super::{desired_legacy_shares, hosts_viewed_window, remote_extents};

    /// A host window (a terminal) streams to each member of its room who
    /// views it, read only, while the room shares its layout: not while
    /// nobody views it, not to a viewer in another room, and not at all in a
    /// room whose layout is personal.
    #[test]
    fn a_host_window_streams_read_only_to_room_members_viewing_it_in_a_shared_room() {
        use crate::shell::network::RoomWindowViewer;
        use crate::shell::state::{
            RoomId, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree,
        };
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.desktop.workspaces[0].strips.truncate(1);
        tree.desktop.workspaces[0].strips[0].window_groups = vec![WindowGroup {
            id: 91,
            tabs: vec![Tab {
                id: 93,
                title: "ssh prod-db".into(),
                tile: TileContent::Foreign {
                    app_id: "Alacritty".into(),
                    surface_id: 31,
                },
                app_title: None,
            }],
            active_tab: 93,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }];
        let room = tree.desktop.workspaces[0].room();
        let member = |peer: u64, room: RoomId, width: u32, height: u32| RoomWindowViewer {
            peer,
            room,
            width,
            height,
            decode_limits: std::collections::BTreeMap::new(),
        };
        let members = MemberViewers::from([(
            93,
            vec![
                member(601, room, 1400, 900),
                member(602, RoomId::new(), 300, 300),
            ],
        )]);
        let none = RoomHosts::new();
        let nobody = std::collections::BTreeMap::new();

        let desired = desired_legacy_shares(&tree, true, &none, &nobody, &members);
        let (share, shared_room) = desired.get(&31).expect("the terminal streams");
        assert_eq!(*shared_room, room);
        assert_eq!(share.viewers, [601].into(), "only the member of its room");
        assert_eq!(share.read_only, [601].into(), "who only watches");
        assert!(hosts_viewed_window(&tree, &nobody, &members));
        assert_eq!(
            remote_extents(&desired, &nobody, &members).get(&31),
            Some(&(1400, 900)),
            "sized for the member showing it"
        );

        assert!(
            desired_legacy_shares(&tree, true, &none, &nobody, &MemberViewers::new()).is_empty(),
            "nothing is encoded while nobody views it"
        );

        let mut personal = automerge::AutoCommit::new();
        automerge::transaction::Transactable::put(
            &mut personal,
            automerge::ROOT,
            tilo_desktop_model::projection::ROOM_LAYOUT_KEY,
            tilo_desktop_model::projection::RoomLayout::Personal.id(),
        )
        .unwrap();
        tree.room_docs.insert(room, personal);
        assert!(
            desired_legacy_shares(&tree, true, &none, &nobody, &members).is_empty(),
            "a room whose layout is personal streams nothing"
        );
    }
}
