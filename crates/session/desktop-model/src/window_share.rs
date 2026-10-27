//! A window one member shares with a room whose layout is personal.
//!
//! In a personal room each member's windows stay on their device
//! ([`crate::projection::RoomLayout::Personal`]). A member who wants the
//! others to see one of them shares it, and the share is the room's
//! resource share ([`tilo_action_policy::share`]) of the window's own node,
//! `room › <app> › window › <tab>`: grants the room's governor writes into
//! its governance at the sharer's request, naming who gave them
//! (`granted_by`) and until when (`lifetime`). The governor takes such a
//! share only from the device the tab's topology namespace is registered to
//! (`contents_owners_v1`), so the registry says which device hosts the
//! window, and nobody shares another device's window.
//!
//! Every device reads the room's live window shares from the room's
//! governance ([`window_shares`]): the sharer streams only those windows to
//! the room, and every other member shows each as a tab of the sharer's
//! window in its own layout, for as long as the share lasts.

use std::collections::BTreeMap;

use tilo_action_policy::share::share_id_prefix;
use tilo_action_policy::{
    ActionKind, Answer, Context, Level, RoomPolicy, Segment, Subject, TreePath,
};

use crate::ids::{TabId, peer_salt_of};
use crate::projection::{contents_key, contents_key_id};

/// The resource kind a window's node names under its app.
pub const WINDOW_KIND: &str = "window";

/// The share id of sharing the window `tab`: one share per window, so
/// sharing it again is the same share and stopping ends it whole.
#[must_use]
pub fn window_share_id(tab: TabId) -> String {
    format!("{WINDOW_KIND}.{}", contents_key(tab))
}

/// The node of the window `tab` of `app_id` in `room`: what a share of it
/// grants on.
#[must_use]
pub fn window_node(room: &str, app_id: &str, tab: TabId) -> TreePath {
    TreePath::room(room)
        .app(app_id)
        .kind(WINDOW_KIND)
        .resource(contents_key(tab))
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
    let tab = contents_key_id(named(segments.get(3), Level::Resource)?)?;
    Some((app.to_owned(), tab))
}

/// One window shared with a room, as its governance records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowShare {
    /// The sharer's tab, in the topology namespace of the device hosting it.
    pub tab: TabId,
    /// The app the window belongs to.
    pub app_id: String,
    /// The person who shared it.
    pub sharer: String,
    /// The hex device key of the device hosting the window: the one its
    /// tab's namespace is registered to.
    pub host: String,
    /// Until when the share lasts, in Unix milliseconds; `None` until the
    /// sharer stops it.
    pub until_ms: Option<u64>,
}

/// The windows shared with `policy`'s room that are live at `now_ms`, by
/// tab, each hosted on the device `owners` (the room's topology registry,
/// [`crate::projection::contents_owners_of`]) registers its tab's namespace
/// to. A share counts when it is a share of one window's node under that
/// window's share id, given by a person, allowing the room to see it; a
/// window whose namespace no device holds is nobody's to show.
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
        });
    }
    shares
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::share::{ShareAccess, share_grants};

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
            })
        );
        assert!(
            window_shares(&policy, &BTreeMap::new(), 1).is_empty(),
            "a window no device's namespace holds is nobody's to show"
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
            .resource(contents_key(tab));
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
}
