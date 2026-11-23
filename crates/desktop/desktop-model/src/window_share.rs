//! A window one member shares with the workspace. A Tilo app's window is the
//! workspace's already: every device opens it on the record it shows. A window
//! whose state lives in its process on one device shows its contents to the
//! others only once its member shares it, and the share is the workspace's
//! resource share ([`tilo_action_policy::share`]) of the window's own node. The
//! governor takes such a share only from the device that minted the tab's id, so
//! nobody shares another device's window. A window that closes ends its share as
//! it goes: the grants are in the same workspace the close already changes.

use std::collections::BTreeMap;

use tilo_action_policy::share::{is_of_share, share_id_prefix};
use tilo_action_policy::{
    ActionKind, Answer, Context, GrantId, Level, Segment, TreePath, WorkspacePolicy,
};

use crate::ids::{TabId, device_salt_of_key, peer_salt_of};
use crate::layout::TileContent;
use crate::tree::Workspace;

/// Key of the workspace's governed device identities, by device key.
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

/// The salts the devices `doc`, a workspace's document, admitted mint under, and
/// each one's device key (64 lowercase hex digits): what tells which device
/// a window of the workspace runs on. A salt two of its devices share is
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

/// A window a device maps in a tab that stands in a workspace: what that device can share with
/// the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MappedWindow<'a> {
    /// The app the window belongs to.
    pub app_id: &'a str,
    /// The surface the device's compositor maps the window under.
    pub surface_id: u64,
    /// The workspace the tab stands in, by its id.
    pub workspace: &'a str,
}

/// The window the device holding `workspaces` maps in `tab`. `None` for a tab the shell does not
/// hold, one in no workspace, and one that shows no window this device maps: the picture of
/// another device's window, and an app whose window has yet to open. Only such a window is shared
/// from this device, so a shell offers the share and its session carries it out by this one rule.
#[must_use]
pub fn mapped_window(workspaces: &[Workspace], tab: TabId) -> Option<MappedWindow<'_>> {
    let (held, shown) = workspaces.iter().find_map(|held| {
        held.tabs()
            .find(|shown| shown.id == tab)
            .map(|shown| (held, shown))
    })?;
    let TileContent::Foreign { app_id, surface_id } = &shown.tile else {
        return None;
    };
    if held.policy.workspace.is_empty() {
        return None;
    }
    Some(MappedWindow {
        app_id,
        surface_id: *surface_id,
        workspace: &held.policy.workspace,
    })
}

/// The resource kind a window's node names under its app.
pub const WINDOW_KIND: &str = "window";

/// The share id of sharing the window `tab`: one share per window, so
/// sharing it again is the same share and stopping ends it whole.
#[must_use]
pub fn window_share_id(tab: TabId) -> String {
    format!("{WINDOW_KIND}.{}", tab_key(tab))
}

/// The window a share id names.
#[must_use]
pub fn window_of_share_id(share: &str) -> Option<TabId> {
    let key = share.strip_prefix(WINDOW_KIND)?.strip_prefix('.')?;
    let tab = tab_of_key(key)?;
    (tab_key(tab) == key).then_some(tab)
}

/// The node of the window `tab` of `app_id` in `workspace`: what a share of it
/// grants on.
#[must_use]
pub fn window_node(workspace: &str, app_id: &str, tab: TabId) -> TreePath {
    TreePath::workspace(workspace)
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
    if segments.len() != 4 || named(segments.first(), Level::Workspace).is_none() {
        return None;
    }
    let app = named(segments.get(1), Level::App)?;
    if named(segments.get(2), Level::Kind)? != WINDOW_KIND {
        return None;
    }
    let tab = tab_of_key(named(segments.get(3), Level::Resource)?)?;
    Some((app.to_owned(), tab))
}

/// One window shared with a workspace, as its governance records it.
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
}

/// The windows shared with `policy`'s workspace that are live at `now_ms`, by
/// tab, each hosted on the device whose salt its tab carries (`owners`,
/// [`device_salts`] of the workspace's document). A share counts when it is a
/// share of one window's node under that window's share id, given by a
/// person, allowing the workspace to see it; a window no device of the
/// workspace minted is nobody's to show.
#[must_use]
pub fn window_shares(
    policy: &WorkspacePolicy,
    owners: &BTreeMap<u16, String>,
    now_ms: u64,
) -> BTreeMap<TabId, WindowShare> {
    let cx = Context::at(now_ms);
    let mut shares = BTreeMap::new();
    for grant in &policy.grants {
        let Some((app_id, tab)) = window_of_node(&grant.node) else {
            continue;
        };
        let Some(giver) = grant.granted_by.person_id() else {
            continue;
        };
        if grant.node.workspace_id() != Some(policy.workspace.as_str())
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
            sharer: giver.to_owned(),
            host: host.clone(),
            until_ms,
        });
    }
    shares
}

impl Workspace {
    /// The window `tab` is gone: the share of it ends here.
    /// [`Workspace::note_close`] calls it as it applies a close, so the workspace's record stops naming the window the
    /// moment it closes, and a window reopened afterwards carries no share of
    /// the one that went. Answers the grants retracted, for whoever mirrors
    /// the workspace's policy into the workspace's document.
    pub fn end_window_shares(&mut self, tab: TabId) -> Vec<GrantId> {
        let share = window_share_id(tab);
        self.policy
            .revoke_grants_matching(|grant| is_of_share(grant, &share))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::share::{ShareAccess, share_grants};

    use crate::ids::compose_id;

    const WORKSPACE: &str = "6f1d2c3b-4a59-4e8d-9c7b-1a2b3c4d5e6f";

    /// A workspace whose one window shows `tile` in `tab`.
    fn showing(tile: TileContent, tab: TabId) -> Workspace {
        let mut workspace = Workspace::new(crate::WorkspaceId::from_u128(1), "Shell");
        let mut strip = crate::Strip::new(crate::StripId::from(1), "Main");
        strip.window_groups.push(crate::WindowGroup {
            id: crate::GroupId::from(1_u64),
            tabs: vec![crate::Tab {
                id: tab,
                title: "Notes".to_owned(),
                tile,
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            active_tab: tab,
            width: crate::WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        workspace.strips.push(strip);
        workspace
    }

    tilo_test_env::scenario! {
        fn a_window_share_lasts_while_its_window_does;

        phase only_a_window_the_device_maps_in_a_workspace_is_one_it_can_share;
        // A device shares the window it maps in a tab of a workspace, and nothing else a tab can
        // show: not the picture of another device's window, not an app still to open, and nothing
        // from a shell in no workspace.
        {
                let tab = compose_id(7, 1);
                let app = || "so.tilo.notes".to_owned();
                let mapped = showing(
                    TileContent::Foreign {
                        app_id: app(),
                        surface_id: 41,
                    },
                    tab,
                );
                let workspace = mapped.policy.workspace.clone();
                assert!(!workspace.is_empty(), "a workspace is known by its id");
                assert_eq!(
                    mapped_window(std::slice::from_ref(&mapped), tab),
                    Some(MappedWindow {
                        app_id: "so.tilo.notes",
                        surface_id: 41,
                        workspace: &workspace,
                    })
                );
                assert_eq!(
                    mapped_window(std::slice::from_ref(&mapped), compose_id(7, 2)),
                    None,
                    "a tab the shell does not hold shows no window"
                );
                for tile in [
                    TileContent::HostedWindow {
                        app_id: app(),
                        label: "Notes".to_owned(),
                        host: "ab".repeat(32),
                        host_label: "Framework".to_owned(),
                    },
                    TileContent::LocalApp {
                        app_id: app(),
                        label: "Notes".to_owned(),
                        note: String::new(),
                    },
                ] {
                    let other = showing(tile.clone(), tab);
                    assert_eq!(
                        mapped_window(std::slice::from_ref(&other), tab),
                        None,
                        "{tile:?} is no window this device maps"
                    );
                }
                let mut nowhere = mapped;
                nowhere.policy.workspace.clear();
                assert_eq!(
                    mapped_window(std::slice::from_ref(&nowhere), tab),
                    None,
                    "a shell in no workspace has nobody to share with"
                );
        }

        phase a_window_share_names_its_window_its_sharer_and_its_host_while_it_lasts;
        {
            let tab = (0x0A51_u64 << 48) | 4;
            let node = window_node(WORKSPACE, "so.tilo.notes", tab);
            assert_eq!(
                window_of_node(&node),
                Some(("so.tilo.notes".to_owned(), tab))
            );
            let mut policy = WorkspacePolicy::new(WORKSPACE, "owner".to_owned());
            let grants = share_grants(
                WORKSPACE,
                "ana",
                &window_share_id(tab),
                &node,
                ShareAccess::View,
            )
            .expect("a window is one resource of the workspace");
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
                "a window no device of the workspace minted is nobody's to show"
            );

            // A share that ended by its time is no share.
            policy.grants = grants
                .into_iter()
                .map(|grant| grant.with_lifetime(tilo_action_policy::Lifetime::Until(10)))
                .collect();
            assert_eq!(window_shares(&policy, &owners, 9)[&tab].until_ms, Some(10));
            assert!(window_shares(&policy, &owners, 10).is_empty());

            // A share of a record is no window's.
            let record = TreePath::workspace(WORKSPACE)
                .app("so.tilo.notes")
                .kind("notes.notebook")
                .resource(tab_key(tab));
            policy.grants = share_grants(
                WORKSPACE,
                "ana",
                &window_share_id(tab),
                &record,
                ShareAccess::View,
            )
            .expect("a record share");
            assert!(window_shares(&policy, &owners, 1).is_empty());
        }

        phase a_share_id_names_its_window;
        {
            let tab = (0x0A51_u64 << 48) | 4;
            assert_eq!(window_of_share_id(&window_share_id(tab)), Some(tab));
            assert_eq!(window_of_share_id("window.x"), None);
            assert_eq!(window_of_share_id("message.1"), None);
        }

        phase closing_a_window_ends_the_shares_it_showed;
        {
            use crate::commands::{DesktopCommand, TabLocation, apply_command};
            use crate::tree::Layout;

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
                "both windows are shared with the workspace"
            );
            // The person closes the first window            // The person closes the first window: its share ends there.
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
                "the share of the closed window ended, and the workspace still shows the other"
            );

            // The window reopens where it was without the share that went: a
            // window comes back, its share does not, and nobody shares it again
            // but the person.
            let mut tree = Layout {
                active: workspace.id(),
                workspaces: vec![workspace],
                ..Layout::default()
            };
            let active = tree.active;
            assert_eq!(
                tree.reopen_closed(active, None).map(|put| put.tab),
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
    }

    /// The device the two windows below run on, and the workspace's name for it.
    const ANA: u16 = 0x0A51;
    const APP: &str = "so.tilo.notes";
    const NOTES: TabId = compose_id(ANA, 4);
    const TERMINAL: TabId = compose_id(ANA, 5);
    const GROUP: u64 = compose_id(ANA, 6);
    const STRIP: u64 = compose_id(ANA, 7);

    /// A workspace in [`WORKSPACE`] holding one group of two windows, both shared
    /// with the workspace, and the workspace's devices, as its document lists them.
    fn shared_windows() -> (Workspace, BTreeMap<u16, String>) {
        use crate::app::AppKind;
        use crate::ids::{StripId, WorkspaceId};
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
            view: String::new(),
            pinned: false,
        };
        let mut workspace = Workspace::new(
            WorkspaceId::parse(WORKSPACE).expect("a workspace id"),
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
            (window_share_id(NOTES), window_node(WORKSPACE, APP, NOTES)),
            (
                window_share_id(TERMINAL),
                window_node(WORKSPACE, APP, TERMINAL),
            ),
        ] {
            workspace.policy.grants.extend(
                share_grants(WORKSPACE, "ana", &id, &node, ShareAccess::View)
                    .expect("one resource of the workspace"),
            );
        }
        (workspace, BTreeMap::from([(ANA, "aa".to_owned())]))
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
