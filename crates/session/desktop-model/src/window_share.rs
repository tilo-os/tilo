//! A window one member shares with a room whose layout is personal.
//!
//! In a personal room each member's windows stay on their device
//! ([`crate::projection::RoomLayout::Personal`]). A member who wants the
//! others to see one of them shares it, and the share is the room's
//! resource share ([`tilo_action_policy::share`]) of the window's own node,
//! `room › <app> › window › <tab>`: grants the room's governor writes into
//! its governance at the sharer's request, naming who gave them
//! (`granted_by`) and until when (`lifetime`). The governor takes such a
//! share only from the device that minted the tab's id: every device mints
//! under the salt its key derives ([`crate::ids::device_salt_of_key`]), and
//! the room knows its members' device keys ([`device_salts`]), so the tab
//! says which device hosts the window, and nobody shares another device's
//! window.
//!
//! A Tilo app's window is a view of a resource that already lives in the
//! room, so its share also says what the window shows: the share
//! [`window_shows_share_id`] of the resource's own node (the record, or the
//! item of it the window is at). Its sharer streams none of its pixels; every
//! other member opens the same app on that resource. A window with no such
//! share is an outside window (a terminal, any Wayland or X client), whose
//! state lives in its process: it is streamed.
//!
//! Every device reads the room's live window shares from the room's
//! governance ([`window_shares`]): the sharer streams only the outside
//! windows among them to the room, and every other member shows each as a
//! tab of the sharer's window in its own layout, for as long as the share
//! lasts.
//!
//! A window that closes ends its share as it goes
//! ([`Workspace::end_window_shares`]): the grants are in the same workspace
//! the close already changes, so the close retracts them itself, and the
//! room stops naming a window that no longer exists.

use std::collections::BTreeMap;

use tilo_action_policy::share::{is_of_share, share_id_prefix};
use tilo_action_policy::{
    ActionKind, Answer, Context, GrantId, Level, RoomPolicy, Segment, Subject, TreePath,
};

use crate::ids::{TabId, device_salt_of_key, peer_salt_of};
use crate::workspace::Workspace;

/// Key of the room's governed device identities, by device key.
const PEER_IDENTITIES_KEY: &str = "peer_identities_v1";

/// A window id as it stands in a share id or a node: fixed-width hex, so it
/// parses back into the id it names and every device spells it the same.
fn tab_key(tab: TabId) -> String {
    format!("{tab:016x}")
}

/// The window id `key` spells, as [`tab_key`] writes it.
fn tab_of_key(key: &str) -> Option<TabId> {
    u64::from_str_radix(key, 16).ok()
}

/// The salts the devices `doc`, a room's document, admitted mint under, and
/// each one's device key (64 lowercase hex digits): what tells which device
/// a window of the room runs on. A salt two of its devices share is
/// nobody's, so neither can speak for the other's windows.
#[must_use]
pub fn device_salts(doc: &automerge::AutoCommit) -> BTreeMap<u16, String> {
    use automerge::{ROOT, ReadDoc as _};
    let Some((_, identities)) = doc.get(&ROOT, PEER_IDENTITIES_KEY).ok().flatten() else {
        return BTreeMap::new();
    };
    let mut salts: BTreeMap<u16, Option<String>> = BTreeMap::new();
    for key in doc.keys(&identities) {
        let Some(device) = hex::decode(&key).ok().filter(|bytes| bytes.len() == 32) else {
            continue;
        };
        salts
            .entry(device_salt_of_key(&device))
            .and_modify(|held| *held = None)
            .or_insert(Some(key));
    }
    salts
        .into_iter()
        .filter_map(|(salt, device)| Some((salt, device?)))
        .collect()
}

/// The resource kind a window's node names under its app.
pub const WINDOW_KIND: &str = "window";

/// The share id of sharing the window `tab`: one share per window, so
/// sharing it again is the same share and stopping ends it whole.
#[must_use]
pub fn window_share_id(tab: TabId) -> String {
    format!("{WINDOW_KIND}.{}", tab_key(tab))
}

/// The share id of what the window `tab` of a Tilo app shows, beside its
/// own share ([`window_share_id`]). Its grants' ids start with the window
/// share's prefix, so ending the window's share ends this one with it.
#[must_use]
pub fn window_shows_share_id(tab: TabId) -> String {
    format!("{}:{SHOWS}", window_share_id(tab))
}

/// What [`window_shows_share_id`] adds to the window's share id.
const SHOWS: &str = "shows";

/// The window a share id names, and whether it is the share of what the
/// window shows ([`window_shows_share_id`]) rather than of the window.
#[must_use]
pub fn window_of_share_id(share: &str) -> Option<(TabId, bool)> {
    let rest = share.strip_prefix(WINDOW_KIND)?.strip_prefix('.')?;
    let (key, shows) = match rest.split_once(':') {
        Some((key, SHOWS)) => (key, true),
        Some(_) => return None,
        None => (rest, false),
    };
    let tab = tab_of_key(key)?;
    (tab_key(tab) == key).then_some((tab, shows))
}

/// The node of the window `tab` of `app_id` in `room`: what a share of it
/// grants on.
#[must_use]
pub fn window_node(room: &str, app_id: &str, tab: TabId) -> TreePath {
    TreePath::room(room)
        .app(app_id)
        .kind(WINDOW_KIND)
        .resource(tab_key(tab))
}

/// The window `node` names, with its app: `None` for any other node.
#[must_use]
pub fn window_of_node(node: &TreePath) -> Option<(String, TabId)> {
    fn named(segment: Option<&Segment>, level: Level) -> Option<&str> {
        match segment {
            Some(Segment::Name(at, id)) if *at == level && !id.is_empty() => Some(id.as_str()),
            _ => None,
        }
    }
    let segments = node.segments();
    if segments.len() != 4 || named(segments.first(), Level::Room).is_none() {
        return None;
    }
    let app = named(segments.get(1), Level::App)?;
    if named(segments.get(2), Level::Kind)? != WINDOW_KIND {
        return None;
    }
    let tab = tab_of_key(named(segments.get(3), Level::Resource)?)?;
    Some((app.to_owned(), tab))
}

/// What a Tilo app's shared window shows: a record of the room, and the
/// item of it the window is at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowShows {
    /// The record, as its route names it: empty for the app's own record
    /// of the room.
    pub record: String,
    /// What in the record the window is at (a note of the notebook), as
    /// the app's `open` takes it; `None` for the record as a whole.
    pub item: Option<String>,
}

/// The node a share of what the window `tab` of `app_id` shows grants on:
/// the item of the record it is at, or the record (`record`, a route, of
/// kind `kind`). The app's own record as a whole has no node of its own
/// (a node naming every record of the kind is no one resource), so it is
/// named by the window's own node.
#[must_use]
pub fn shows_node(
    room: &str,
    app_id: &str,
    tab: TabId,
    kind: &str,
    shows: &WindowShows,
) -> TreePath {
    let at = TreePath::room(room).app(app_id).kind(kind);
    match (&shows.item, shows.record.is_empty()) {
        // An item of the app's own record: its id is unique, so any
        // record of the kind is it.
        (Some(item), true) => at.any(Level::Resource).item(item.as_str()),
        (Some(item), false) => at.resource(shows.record.as_str()).item(item.as_str()),
        (None, false) => at.resource(shows.record.as_str()),
        (None, true) => window_node(room, app_id, tab),
    }
}

/// What the share of what the window `tab` of `app_id` shows says, from
/// its node ([`shows_node`]): `None` for a node of another app or window,
/// or one that names no record or item.
#[must_use]
pub fn shows_of_node(node: &TreePath, app_id: &str, tab: TabId) -> Option<WindowShows> {
    if window_of_node(node).is_some_and(|(app, window)| app == app_id && window == tab) {
        return Some(WindowShows::default());
    }
    let segments = node.segments();
    let named = |at: usize, level: Level| match segments.get(at) {
        Some(Segment::Name(found, id)) if *found == level && !id.is_empty() => Some(id.clone()),
        _ => None,
    };
    if named(0, Level::Room).is_none()
        || named(1, Level::App).as_deref() != Some(app_id)
        || named(2, Level::Kind)
            .as_deref()
            .is_none_or(|kind| kind == WINDOW_KIND)
    {
        return None;
    }
    let record = match segments.get(3) {
        Some(Segment::Any(Level::Resource)) => String::new(),
        _ => named(3, Level::Resource)?,
    };
    let item = match segments.len() {
        4 if !record.is_empty() => None,
        5 => Some(named(4, Level::Item)?),
        _ => return None,
    };
    Some(WindowShows { record, item })
}

/// One window shared with a room, as its governance records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowShare {
    /// The sharer's tab, minted under the salt of the device hosting it.
    pub tab: TabId,
    /// The app the window belongs to.
    pub app_id: String,
    /// The person who shared it.
    pub sharer: String,
    /// The hex device key of the device hosting the window: the one whose
    /// salt its tab carries.
    pub host: String,
    /// Until when the share lasts, in Unix milliseconds; `None` until the
    /// sharer stops it.
    pub until_ms: Option<u64>,
    /// What the window shows, when it is a Tilo app's: the others open the
    /// same app on it. `None` for an outside window, which is streamed.
    pub shows: Option<WindowShows>,
}

/// The windows shared with `policy`'s room that are live at `now_ms`, by
/// tab, each hosted on the device whose salt its tab carries (`owners`,
/// [`device_salts`] of the room's document). A share counts when it is a
/// share of one window's node under that window's share id, given by a
/// person, allowing the room to see it; a window no device of the room
/// minted is nobody's to show.
#[must_use]
pub fn window_shares(
    policy: &RoomPolicy,
    owners: &BTreeMap<u16, String>,
    now_ms: u64,
) -> BTreeMap<TabId, WindowShare> {
    let cx = Context::at(now_ms);
    let mut shares = BTreeMap::new();
    for grant in &policy.grants {
        let Some((app_id, tab)) = window_of_node(&grant.node) else {
            continue;
        };
        let Subject::Person(giver) = &grant.granted_by else {
            continue;
        };
        if grant.node.room_id() != Some(policy.room.as_str())
            || !grant
                .id
                .as_str()
                .starts_with(&share_id_prefix(&window_share_id(tab)))
            || grant.answer != Answer::Allow
            || !grant.kinds.contains(ActionKind::Read)
            || !grant.is_live(&cx)
        {
            continue;
        }
        let Some(host) = owners.get(&peer_salt_of(tab)) else {
            continue;
        };
        let until_ms = match grant.lifetime {
            tilo_action_policy::Lifetime::Until(at) => Some(at),
            _ => None,
        };
        shares.entry(tab).or_insert_with(|| WindowShare {
            tab,
            app_id,
            sharer: giver.clone(),
            host: host.clone(),
            until_ms,
            shows: None,
        });
    }
    // What each Tilo app's window shows: the share its sharer made beside
    // the window's, of the resource's own node.
    for grant in &policy.grants {
        let Subject::Person(giver) = &grant.granted_by else {
            continue;
        };
        if grant.answer != Answer::Allow
            || !grant.kinds.contains(ActionKind::Read)
            || !grant.is_live(&cx)
            || grant.node.room_id() != Some(policy.room.as_str())
        {
            continue;
        }
        for share in shares.values_mut() {
            if share.shows.is_some()
                || share.sharer != *giver
                || !grant
                    .id
                    .as_str()
                    .starts_with(&share_id_prefix(&window_shows_share_id(share.tab)))
            {
                continue;
            }
            share.shows = shows_of_node(&grant.node, &share.app_id, share.tab);
        }
    }
    shares
}

impl Workspace {
    /// The window `tab` is gone: the share of it ends here, and with it the
    /// share of what it showed ([`window_shows_share_id`] starts with the
    /// window's own share id). [`Workspace::note_close`] calls it as it
    /// applies a close, so the room's record stops naming the window the
    /// moment it closes, and a window reopened afterwards carries no share of
    /// the one that went. Answers the grants retracted, for whoever mirrors
    /// the workspace's policy into the room's document.
    pub fn end_window_shares(&mut self, tab: TabId) -> Vec<GrantId> {
        let share = window_share_id(tab);
        self.revoke_grants_matching(|grant| is_of_share(grant, &share))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::share::{ShareAccess, share_grants};

    use crate::ids::compose_id;

    const ROOM: &str = "6f1d2c3b-4a59-4e8d-9c7b-1a2b3c4d5e6f";

    #[test]
    fn a_window_share_names_its_window_its_sharer_and_its_host_while_it_lasts() {
        let tab = (0x0A51_u64 << 48) | 4;
        let node = window_node(ROOM, "so.tilo.notes", tab);
        assert_eq!(
            window_of_node(&node),
            Some(("so.tilo.notes".to_owned(), tab))
        );
        let mut policy = RoomPolicy::new(ROOM, "owner".to_owned());
        let grants = share_grants(ROOM, "ana", &window_share_id(tab), &node, ShareAccess::View)
            .expect("a window is one resource of the room");
        let owners = BTreeMap::from([(0x0A51, "aa".to_owned())]);
        assert!(window_shares(&policy, &owners, 1).is_empty());
        policy.grants.extend(grants.clone());
        let shares = window_shares(&policy, &owners, 1);
        assert_eq!(
            shares.get(&tab),
            Some(&WindowShare {
                tab,
                app_id: "so.tilo.notes".into(),
                sharer: "ana".into(),
                host: "aa".into(),
                until_ms: None,
                shows: None,
            })
        );
        assert!(
            window_shares(&policy, &BTreeMap::new(), 1).is_empty(),
            "a window no device of the room minted is nobody's to show"
        );

        // A share that ended by its time is no share.
        policy.grants = grants
            .into_iter()
            .map(|grant| grant.with_lifetime(tilo_action_policy::Lifetime::Until(10)))
            .collect();
        assert_eq!(window_shares(&policy, &owners, 9)[&tab].until_ms, Some(10));
        assert!(window_shares(&policy, &owners, 10).is_empty());

        // A share of a record is no window's.
        let record = TreePath::room(ROOM)
            .app("so.tilo.notes")
            .kind("notes.notebook")
            .resource(tab_key(tab));
        policy.grants = share_grants(
            ROOM,
            "ana",
            &window_share_id(tab),
            &record,
            ShareAccess::View,
        )
        .expect("a record share");
        assert!(window_shares(&policy, &owners, 1).is_empty());
    }

    #[test]
    fn a_tilo_apps_window_share_says_what_the_window_shows() {
        let tab = (0x0A51_u64 << 48) | 4;
        let app = "so.tilo.notes";
        assert_eq!(
            window_of_share_id(&window_share_id(tab)),
            Some((tab, false))
        );
        assert_eq!(
            window_of_share_id(&window_shows_share_id(tab)),
            Some((tab, true))
        );
        assert_eq!(window_of_share_id("window.x:shows"), None);
        assert_eq!(window_of_share_id("message.1"), None);
        assert!(
            window_shows_share_id(tab).starts_with(&format!("{}:", window_share_id(tab))),
            "ending the window's share ends what it shows with it"
        );

        let owners = BTreeMap::from([(0x0A51, "aa".to_owned())]);
        let share = |shows: &WindowShows, kind: &str| {
            let mut policy = RoomPolicy::new(ROOM, "owner".to_owned());
            for (id, node) in [
                (window_share_id(tab), window_node(ROOM, app, tab)),
                (
                    window_shows_share_id(tab),
                    shows_node(ROOM, app, tab, kind, shows),
                ),
            ] {
                policy.grants.extend(
                    share_grants(ROOM, "ana", &id, &node, ShareAccess::View)
                        .expect("one resource of the room"),
                );
            }
            window_shares(&policy, &owners, 1)
                .remove(&tab)
                .expect("the window is shared")
        };
        for shows in [
            // A note of the app's own notebook.
            WindowShows {
                record: String::new(),
                item: Some("notes.page/pages/7".into()),
            },
            // A record of its own, and an item of it.
            WindowShows {
                record: "derived:default@tab:0a51000000000009".into(),
                item: None,
            },
            WindowShows {
                record: "derived:default@tab:0a51000000000009".into(),
                item: Some("7".into()),
            },
            // The app's own record as a whole.
            WindowShows::default(),
        ] {
            assert_eq!(share(&shows, "notes.notebook").shows, Some(shows));
        }

        // A window with nothing shown beside it is an outside window's.
        let mut policy = RoomPolicy::new(ROOM, "owner".to_owned());
        policy.grants.extend(
            share_grants(
                ROOM,
                "ana",
                &window_share_id(tab),
                &window_node(ROOM, app, tab),
                ShareAccess::View,
            )
            .expect("a window"),
        );
        assert_eq!(window_shares(&policy, &owners, 1)[&tab].shows, None);

        // What another person shares under the window's id is not what it shows.
        policy.grants.extend(
            share_grants(
                ROOM,
                "bo",
                &window_shows_share_id(tab),
                &TreePath::room(ROOM)
                    .app(app)
                    .kind("notes.notebook")
                    .resource("r"),
                ShareAccess::View,
            )
            .expect("a record"),
        );
        assert_eq!(window_shares(&policy, &owners, 1)[&tab].shows, None);
        // Nor is a node of another app.
        assert_eq!(
            shows_of_node(
                &TreePath::room(ROOM)
                    .app("so.tilo.chat")
                    .kind("k")
                    .resource("r"),
                app,
                tab
            ),
            None
        );
    }

    /// The device the two windows below run on, and the room's name for it.
    const ANA: u16 = 0x0A51;
    const APP: &str = "so.tilo.notes";
    const NOTES: TabId = compose_id(ANA, 4);
    const TERMINAL: TabId = compose_id(ANA, 5);
    const GROUP: u64 = compose_id(ANA, 6);
    const STRIP: u64 = compose_id(ANA, 7);

    /// A workspace in [`ROOM`] holding one group of two windows, both shared
    /// with the room — the first with the note it shows beside its own share
    /// — and the room's devices, as its document lists them.
    fn shared_windows() -> (Workspace, BTreeMap<u16, String>) {
        use crate::app::AppKind;
        use crate::ids::{RoomId, StripId, WorkspaceId};
        use crate::layout::{Strip, Tab, TileContent, WindowGroup};
        use crate::width::WindowWidth;

        let window = |id: TabId, title: &str| Tab {
            id,
            title: title.into(),
            tile: TileContent::AppPlaceholder {
                kind: AppKind::Other(APP.into()),
                label: title.into(),
            },
            app_title: None,
        };
        let mut workspace = Workspace::in_room(
            WorkspaceId(1),
            RoomId::parse(ROOM).expect("a room id"),
            "Team",
        );
        workspace.strips = vec![Strip {
            id: StripId(STRIP),
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: GROUP,
                tabs: vec![window(NOTES, "Notes"), window(TERMINAL, "Terminal")],
                active_tab: NOTES,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }],
        }];
        for (id, node) in [
            (window_share_id(NOTES), window_node(ROOM, APP, NOTES)),
            (
                window_shows_share_id(NOTES),
                TreePath::room(ROOM)
                    .app(APP)
                    .kind("notes.notebook")
                    .any(Level::Resource)
                    .item("notes.page/pages/7"),
            ),
            (window_share_id(TERMINAL), window_node(ROOM, APP, TERMINAL)),
        ] {
            workspace.policy.grants.extend(
                share_grants(ROOM, "ana", &id, &node, ShareAccess::View)
                    .expect("one resource of the room"),
            );
        }
        (workspace, BTreeMap::from([(ANA, "aa".to_owned())]))
    }

    #[test]
    fn closing_a_window_ends_the_shares_it_showed() {
        use crate::commands::{DesktopCommand, TabLocation, apply_command};
        use crate::tree::DesktopTree;

        let (mut workspace, owners) = shared_windows();
        let shared = |workspace: &Workspace| {
            window_shares(&workspace.policy, &owners, 1)
                .keys()
                .copied()
                .collect::<Vec<TabId>>()
        };
        assert_eq!(
            shared(&workspace),
            [NOTES, TERMINAL],
            "both windows are shared with the room"
        );
        assert_eq!(
            window_shares(&workspace.policy, &owners, 1)[&NOTES].shows,
            Some(WindowShows {
                record: String::new(),
                item: Some("notes.page/pages/7".into()),
            }),
            "the first window is shared as the note it is at"
        );

        // The person closes the first window: its share ends there, and what
        // it showed goes with it.
        let mut mint = || Ok(0_u64);
        let mut create = |_: &str, _: u64| Err("a close opens no record".to_owned());
        assert_eq!(
            apply_command(
                &mut workspace,
                &DesktopCommand::CloseTab {
                    location: TabLocation {
                        strip: STRIP,
                        group: GROUP,
                        tab: NOTES,
                    },
                },
                &mut mint,
                &mut create,
            ),
            Ok(())
        );
        assert_eq!(
            shared(&workspace),
            [TERMINAL],
            "the share of the closed window ended, and the room still shows the other"
        );

        // The window reopens where it was without the share that went: a
        // window comes back, its share does not, and nobody shares it again
        // but the person.
        let mut tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let active = tree.active;
        assert_eq!(
            tree.reopen_closed(active).map(|put| put.tab),
            Some(NOTES),
            "the window is back"
        );
        assert_eq!(
            shared(&tree.workspaces[0]),
            [TERMINAL],
            "a reopened window carries no share of the one that went"
        );

        // A window closed from its own bar ends its share the same way.
        assert!(tree.close_tab(TERMINAL).is_some());
        assert!(shared(&tree.workspaces[0]).is_empty());
    }

    #[test]
    fn a_window_runs_on_the_device_whose_key_its_salt_comes_from() {
        use automerge::transaction::Transactable as _;
        let ana = [1_u8; 32];
        let bo = [2_u8; 32];
        let mut doc = automerge::AutoCommit::new();
        assert!(device_salts(&doc).is_empty(), "no devices, no hosts");
        let identities = doc
            .put_object(
                automerge::ROOT,
                PEER_IDENTITIES_KEY,
                automerge::ObjType::Map,
            )
            .unwrap();
        for device in [ana, bo] {
            doc.put(&identities, hex::encode(device), "{}").unwrap();
        }
        doc.put(&identities, "not a key", "{}").unwrap();
        let salts = device_salts(&doc);
        assert_eq!(salts.len(), 2, "{salts:?}");
        assert_eq!(salts[&device_salt_of_key(&ana)], hex::encode(ana));
        assert_eq!(salts[&device_salt_of_key(&bo)], hex::encode(bo));
    }
}
