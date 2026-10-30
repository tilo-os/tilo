//! Which legacy windows stream to whom: this device's windows to the
//! devices viewing them, and the windows hosted on other devices that this
//! device shows, which it asks their hosts to stream it, as one of the
//! person's devices or as a member of the room.

use std::collections::HashMap;

use super::{DesiredShares, MemberViewers, PersonalViewers, RoomHosts, all_hosts};

/// The legacy windows to encode for viewers: none while `watched` is false,
/// when neither the backend nor any viewer below is looking, so a headless
/// machine's encoders stop and receivers get `SurfaceStreamEnd` at once.
///
/// `viewer_hosts` are each room's workspace hosts when the backend is
/// headless: its viewers watch through that host (which decides which of
/// them see and control what), so every legacy window of a workspace whose
/// room has one is shared with it, not only the explicitly shared. A headless
/// machine's "active" workspace is nobody's screen, so every room counts; a
/// room's windows go only to that room's hosts. A host republishes what it is
/// shared to every member of its room, so a room whose layout is personal
/// shares with its hosts only the windows shared with the room on purpose
/// ([`crate::shell::state::WorkspaceTree::shared_windows`]).
///
/// `personal` are the person's other devices showing a tab, by tab: a legacy
/// window streams to each device showing its tab, in whichever workspace it
/// is, since those devices show their own choice of workspace.
///
/// `members` are the room members viewing a tab, by tab: a window streams to
/// each member of its room who views it, read only, while the room shares
/// its layout; a room whose layout is personal shares only the windows
/// shared with it on purpose, so none of its other windows streams to
/// anyone but the person's own devices.
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
    let now_ms = tilo_clock::unix_now_ms();
    let mut desired = HashMap::new();
    for workspace in &tree.desktop.workspaces {
        let active = workspace.id == tree.desktop.active;
        let shared_layout = !tree.room_layout(workspace.room()).is_personal();
        for tab in workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
        {
            let crate::shell::state::TileContent::Foreign { app_id, surface_id } = &tab.tile else {
                continue;
            };
            // Each member's windows in a room whose layout is personal stay on
            // their device: no host or member of the room is shown them, but
            // for an outside window its person shared with the room. A Tilo
            // app's window is shared as what it shows, never as pixels.
            let shared_layout =
                shared_layout || tree.streams_shared_window(workspace.room(), tab.id, now_ms);
            let mut peers = std::collections::HashSet::new();
            if active
                && let Some(shared) = workspace
                    .shared_surfaces
                    .iter()
                    .find_map(|(surface, peers)| (surface == *surface_id).then(|| peers.clone()))
            {
                peers.extend(shared);
            }
            if shared_layout && let Some(room_hosts) = viewer_hosts.get(&workspace.room()) {
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
            if shared_layout && let Some(viewers) = members.get(&tab.id) {
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

/// The rooms in which this session hosts a legacy window the room is shown
/// (every one while the room shares its layout, else those shared with it):
/// their carriers are kept, so members that reach this device only through
/// a room's relay view the windows there.
pub(super) fn rooms_hosting_windows(
    tree: &crate::shell::state::WorkspaceTree,
) -> std::collections::BTreeSet<tilo_desktop_model::RoomId> {
    let now_ms = tilo_clock::unix_now_ms();
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| {
            let shared_layout = !tree.room_layout(workspace.room()).is_personal();
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .any(|tab| {
                    matches!(tab.tile, crate::shell::state::TileContent::Foreign { .. })
                        && (shared_layout
                            || tree.streams_shared_window(workspace.room(), tab.id, now_ms))
                })
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
    let now_ms = tilo_clock::unix_now_ms();
    let hosted = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| {
            let shared_layout = !tree.room_layout(workspace.room()).is_personal();
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter(move |tab| {
                    shared_layout || tree.is_window_shared(workspace.room(), tab.id, now_ms)
                })
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
) -> Vec<(String, tilo_device_fabric::WindowView)> {
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
        value
            .round()
            .clamp(1.0, f64::from(tilo_device_fabric::MAX_WINDOW_VIEW_SIDE)) as u32
    };
    tiles
        .iter()
        .filter(|(_, rect)| rect.width() >= 1.0 && rect.height() >= 1.0)
        .filter_map(|(tab, rect)| {
            let host = hosted.get(tab)?;
            Some((
                (*host).to_owned(),
                tilo_device_fabric::WindowView {
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
    use crate::shell::network::RoomWindowViewer;
    use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree};

    /// A host window (a terminal) streams to each member of its room who
    /// views it, read only, while the room shares its layout: not while
    /// nobody views it, not to a viewer in another room, and not at all in a
    /// room whose layout is personal.
    #[test]
    fn a_host_window_streams_read_only_to_room_members_viewing_it_in_a_shared_room() {
        use crate::shell::state::RoomId;
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
        let hosts: RoomHosts = [(room, [42].into_iter().collect())].into_iter().collect();
        assert_eq!(
            desired_legacy_shares(&tree, true, &hosts, &nobody, &MemberViewers::new())
                .get(&31)
                .map(|(share, _)| share.viewers.clone()),
            Some([42].into()),
            "a headless session's room host is shared the window while the room shares its layout"
        );
        tree.room_docs.insert(room, personal);
        assert!(
            desired_legacy_shares(&tree, true, &none, &nobody, &members).is_empty(),
            "a room whose layout is personal streams nothing"
        );
        // A host republishes what it is shared to every member of its room
        // (and a share naming it is sent on the room's topic, which every
        // member receives), so it is shown none of a personal room's windows.
        assert!(
            desired_legacy_shares(&tree, true, &hosts, &nobody, &members).is_empty(),
            "a room whose layout is personal streams nothing to its hosts either"
        );
    }

    /// A tree of one workspace whose one window holds a tab for each of
    /// `apps` (surfaces 31 and 32), the one at `active` shown, all minted on
    /// the device whose key salts them; their tab ids come with it.
    fn tree_of_two_windows(apps: [&str; 2], active: usize) -> ([u64; 2], WorkspaceTree) {
        // The windows run on the device whose key mints under this salt.
        let salt = tilo_desktop_model::ids::device_salt_of_key(&[0x5a; 32]);
        let own = |counter: u64| (u64::from(salt) << 48) | counter;
        let tabs = [own(3), own(4)];
        let mut tree = WorkspaceTree::bootstrap_default_with_salt(salt);
        tree.desktop.workspaces[0].strips.truncate(1);
        tree.desktop.workspaces[0].strips[0].window_groups = vec![WindowGroup {
            id: own(2),
            tabs: tabs
                .into_iter()
                .zip(apps)
                .zip([31, 32])
                .map(|((id, app), surface)| Tab {
                    id,
                    title: app.into(),
                    tile: TileContent::Foreign {
                        app_id: app.into(),
                        surface_id: surface,
                    },
                    app_title: None,
                })
                .collect(),
            active_tab: tabs[active],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }];
        (tabs, tree)
    }

    /// Device 601 viewing each of `tabs`, and device 42 hosting `room`.
    fn viewed_by_member_and_host(
        room: crate::shell::state::RoomId,
        tabs: [u64; 2],
    ) -> (MemberViewers, RoomHosts) {
        let viewer = |peer: u64| RoomWindowViewer {
            peer,
            room,
            width: 800,
            height: 600,
            decode_limits: std::collections::BTreeMap::new(),
        };
        let members = MemberViewers::from(tabs.map(|tab| (tab, vec![viewer(601)])));
        let hosts: RoomHosts = [(room, [42].into_iter().collect())].into_iter().collect();
        (members, hosts)
    }

    /// The document of a room whose layout is personal, `room_text`, with
    /// the identity of the device the windows run on.
    fn personal_room_document(room_text: &str) -> automerge::AutoCommit {
        use automerge::transaction::Transactable as _;
        use tilo_desktop_model::projection;

        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, projection::ROOM_ID_KEY, room_text)
            .unwrap();
        doc.put(
            automerge::ROOT,
            projection::ROOM_LAYOUT_KEY,
            projection::RoomLayout::Personal.id(),
        )
        .unwrap();
        let identities = doc
            .put_object(
                automerge::ROOT,
                "peer_identities_v1",
                automerge::ObjType::Map,
            )
            .unwrap();
        doc.put(&identities, hex::encode([0x5a; 32]), "{}").unwrap();
        doc
    }

    /// The boundary of a room whose layout is personal: none of a member's
    /// windows streams to the room's members or hosts, however many view
    /// it, but for the one the member shared with the room on purpose; and
    /// that one only while the share lasts.
    #[test]
    fn in_a_personal_room_only_a_window_shared_on_purpose_streams_to_members_or_hosts() {
        use tilo_desktop_model::projection;
        use tilo_desktop_model::window_share::{window_node, window_share_id};

        let ([chat, notes], mut tree) = tree_of_two_windows(["so.tilo.chat", "so.tilo.notes"], 0);
        let room = tree.desktop.workspaces[0].room();
        // B (601) views both of A's windows, as a device that showed them
        // would ask; the room's host (42) watches every room's windows.
        let (members, hosts) = viewed_by_member_and_host(room, [chat, notes]);
        let nobody = std::collections::BTreeMap::new();

        let room_text = room.to_string();
        let mut doc = personal_room_document(&room_text);
        doc.commit();
        tree.project_room_document_for_test(room, &doc);
        assert!(
            desired_legacy_shares(&tree, true, &hosts, &nobody, &members).is_empty(),
            "no unshared window streams to a member or a host"
        );

        let mut policy = projection::room_policy_of(&doc, &room_text);
        policy.grants.extend(
            tilo_action_policy::share::share_grants(
                &room_text,
                "ana",
                &window_share_id(notes),
                &window_node(&room_text, "so.tilo.notes", notes),
                tilo_action_policy::share::ShareAccess::View,
            )
            .unwrap(),
        );
        projection::project_room_policy(&mut doc, &policy).unwrap();
        doc.commit();
        tree.project_room_document_for_test(room, &doc);
        let desired = desired_legacy_shares(&tree, true, &hosts, &nobody, &members);
        assert!(
            !desired.contains_key(&31),
            "Chat, not shared, reaches nobody"
        );
        let (share, shared_room) = desired.get(&32).expect("the shared Notes streams");
        assert_eq!(*shared_room, room);
        assert_eq!(share.tab, notes);
        assert_eq!(
            share.viewers,
            [601, 42].into(),
            "to the member viewing it and the host"
        );
        assert_eq!(share.read_only, [601].into(), "the member only watches");
        assert_eq!(
            super::rooms_hosting_windows(&tree),
            [room].into(),
            "the room's carrier is kept for the shared window"
        );

        policy.grants.clear();
        projection::project_room_policy(&mut doc, &policy).unwrap();
        doc.commit();
        tree.project_room_document_for_test(room, &doc);
        assert!(
            desired_legacy_shares(&tree, true, &hosts, &nobody, &members).is_empty(),
            "once the share ends, nothing streams again"
        );
        assert!(super::rooms_hosting_windows(&tree).is_empty());
    }

    /// The boundary for a Tilo app's window shared with a room whose layout
    /// is personal: it is shared as what it shows, so none of its pixels
    /// streams to the room's members or hosts, while an outside window
    /// shared beside it still does.
    #[test]
    fn a_shared_tilo_app_window_streams_nothing_to_members_or_hosts() {
        use tilo_desktop_model::projection;
        use tilo_desktop_model::window_share::{
            WindowShows, shows_node, window_node, window_share_id, window_shows_share_id,
        };

        let ([terminal, notes], mut tree) =
            tree_of_two_windows(["org.alacritty.Alacritty", "so.tilo.notes"], 1);
        tree.remember_package_launch(notes, "so.tilo.notes".into());
        let room = tree.desktop.workspaces[0].room();
        let (members, hosts) = viewed_by_member_and_host(room, [terminal, notes]);
        let nobody = std::collections::BTreeMap::new();

        let room_text = room.to_string();
        let mut doc = personal_room_document(&room_text);
        let mut policy = projection::room_policy_of(&doc, &room_text);
        let note = WindowShows {
            record: String::new(),
            item: Some("7".into()),
        };
        for (id, node) in [
            (
                window_shows_share_id(notes),
                shows_node(&room_text, "so.tilo.notes", notes, "notes.notebook", &note),
            ),
            (
                window_share_id(notes),
                window_node(&room_text, "so.tilo.notes", notes),
            ),
            (
                window_share_id(terminal),
                window_node(&room_text, "org.alacritty.Alacritty", terminal),
            ),
        ] {
            policy.grants.extend(
                tilo_action_policy::share::share_grants(
                    &room_text,
                    "ana",
                    &id,
                    &node,
                    tilo_action_policy::share::ShareAccess::View,
                )
                .unwrap(),
            );
        }
        projection::project_room_policy(&mut doc, &policy).unwrap();
        doc.commit();
        tree.project_room_document_for_test(room, &doc);
        let now_ms = tilo_clock::unix_now_ms();
        assert!(tree.is_window_shared(room, notes, now_ms));

        let desired = desired_legacy_shares(&tree, true, &hosts, &nobody, &members);
        assert!(
            !desired.contains_key(&32),
            "no pixel of the shared Notes reaches a member or a host"
        );
        let (share, _) = desired
            .get(&31)
            .expect("the shared terminal, an outside window, streams");
        assert_eq!(share.viewers, [601, 42].into());
    }
}
