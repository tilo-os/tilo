//! Who may close, move or rename what is on a desktop a room shares.
//!
//! A shared desktop keeps, beside its tree, who made each strip and opened
//! each window (`authors`, by strip or tab id). People close, move and
//! rename what they made themselves as an ordinary edit of the desktop.
//! Doing it to what someone else made acts on other people's content, which
//! is moderation ([`tilo_action_policy::roles::MODERATION_FIELD`]): it is
//! decided on that strip's or window's `moderation` node, which the
//! Moderator and Owner roles allow and an Editor's denies, and which a room
//! can grant anyone ([`act_node`]). What nobody made (a strip the room
//! itself lists) is nobody's own, so only moderation changes it.
//!
//! [`foreign_acts`] names what one desktop value did to another's strips
//! and windows that its writer did not make. A relay decides every write of
//! a shared desktop with it, and a desktop asks [`may_rearrange`] before it
//! offers to close or move a window, so the offer and the decision agree.

use std::collections::{BTreeMap, HashMap};
use tilo_sort::CompactSort as _;

use serde::{Deserialize, Serialize};
use tilo_action_policy::roles::MODERATION_FIELD;
use tilo_action_policy::{Level, TreePath};

use crate::ids::{GroupId, StripId, TabId, WorkspaceId};
use crate::tree::DesktopTree;

/// The app a room's shared desktop is published under.
pub const DESKTOP_APP: &str = "so.tilo.browser-desktop";
/// The record of [`DESKTOP_APP`] holding a room's shared desktop.
pub const DESKTOP_RECORD: &str = "desktop";
/// The resource kind of a shared desktop in the published tree.
pub const DESKTOP_KIND: &str = "tilo.desktop";

/// Who made each strip and opened each window of a shared desktop, by strip
/// or tab id, as person ids.
pub type Authors = BTreeMap<u64, String>;

/// A shared desktop as a room holds it: the tree and who made what.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SharedDesktop {
    #[serde(deserialize_with = "DesktopTree::deserialize_via_json")]
    pub tree: DesktopTree,
    /// The record each window shows, by tab, for the windows bound to one.
    #[serde(default)]
    pub records: Vec<(TabId, String)>,
    #[serde(default)]
    pub authors: Authors,
}

impl SharedDesktop {
    /// The shared desktop in the JSON text a room's desktop record holds.
    ///
    /// # Errors
    /// The text is not a shared desktop.
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|error| format!("read the shared desktop: {error}"))
    }
}

/// What a write did to a strip or window someone else made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Act {
    /// Closed the window, or removed the strip.
    Close,
    /// Moved the window to another group, strip or workspace.
    Move,
    /// Reordered the window among other people's windows in its strip.
    Reorder,
    /// Renamed the strip.
    Rename,
    /// Changed who it says made it.
    Attribute,
}

impl Act {
    /// The action's id under the item's `moderation` node.
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::Move | Self::Reorder => "move",
            Self::Rename => "rename",
            Self::Attribute => "attribute",
        }
    }
}

/// One strip or window a write acted on that its writer did not make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Touched {
    /// The strip or tab id.
    pub id: u64,
    pub act: Act,
    /// Who made it; `None` for what nobody made.
    pub owner: Option<String>,
}

/// The node `touched` is decided on in `room`:
/// `room › desktop app › desktop › desktop › item › moderation › act`.
#[must_use]
pub fn act_node(room: &str, touched: &Touched) -> TreePath {
    TreePath::room(room)
        .app(DESKTOP_APP)
        .kind(DESKTOP_KIND)
        .resource(DESKTOP_RECORD)
        .item(touched.id.to_string())
        .field(MODERATION_FIELD)
        .action(touched.act.verb())
}

/// Every strip's and window's moderation on `room`'s shared desktop: what a
/// desktop asks the room whether its person may do before it offers to
/// close or move what others made. A grant on one window alone does not
/// cover it; the room's moderators' does.
#[must_use]
pub fn moderation_node(room: &str) -> TreePath {
    TreePath::room(room)
        .app(DESKTOP_APP)
        .kind(DESKTOP_KIND)
        .resource(DESKTOP_RECORD)
        .any(Level::Item)
        .field(MODERATION_FIELD)
        .any(Level::Action)
}

/// Whether `person` made the strip or window `id`.
#[must_use]
pub fn made_by(authors: &Authors, id: u64, person: &str) -> bool {
    authors.get(&id).is_some_and(|author| author == person)
}

/// Where a window is: its workspace, strip and group.
type Place = (WorkspaceId, StripId, GroupId);

struct Index {
    tabs: HashMap<TabId, Place>,
    strips: HashMap<StripId, String>,
}

fn index(tree: &DesktopTree) -> Index {
    let mut tabs = HashMap::new();
    let mut strips = HashMap::new();
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            strips.insert(strip.id, strip.name.clone());
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    tabs.insert(tab.id, (workspace.id, strip.id, group.id));
                }
            }
        }
    }
    Index { tabs, strips }
}

/// What `after` did to the strips and windows of `before` that `writer` did
/// not make, each once, in the order `before` holds them: windows closed or
/// moved, strips removed or renamed, and anything whose maker changed. A
/// window `writer` opened, or one opened since `before`, is theirs to
/// arrange; so is the order of their own windows among others', since a
/// state cannot tell moving one's own window past another from the reverse.
#[must_use]
pub fn foreign_acts(before: &SharedDesktop, after: &SharedDesktop, writer: &str) -> Vec<Touched> {
    let foreign = |id: u64| !made_by(&before.authors, id, writer);
    let owner = |id: u64| before.authors.get(&id).cloned();
    let was = index(&before.tree);
    let now = index(&after.tree);
    let mut touched: Vec<Touched> = Vec::new();
    let mut note = |id: u64, act: Act| {
        // A window moved away is not also reordered where it was.
        let noted = |t: &Touched| t.id == id && (t.act == act || act == Act::Reorder);
        if !touched.iter().any(noted) {
            touched.push(Touched {
                id,
                act,
                owner: owner(id),
            });
        }
    };
    for workspace in &before.tree.workspaces {
        for strip in &workspace.strips {
            if foreign(strip.id.0) {
                match now.strips.get(&strip.id) {
                    None => note(strip.id.0, Act::Close),
                    Some(name) if *name != strip.name => note(strip.id.0, Act::Rename),
                    Some(_) => {}
                }
            }
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    if !foreign(tab.id) {
                        continue;
                    }
                    match now.tabs.get(&tab.id) {
                        None => note(tab.id, Act::Close),
                        Some(place) if was.tabs.get(&tab.id) != Some(place) => {
                            note(tab.id, Act::Move);
                        }
                        Some(_) => {}
                    }
                }
            }
            // Other people's windows keep their order among themselves.
            let Some(later) = after
                .tree
                .workspaces
                .iter()
                .flat_map(|w| &w.strips)
                .find(|s| s.id == strip.id)
            else {
                continue;
            };
            // Groups holding someone else's window, as `before` holds them,
            // that are still in the strip.
            let others: Vec<GroupId> = strip
                .window_groups
                .iter()
                .filter(|g| g.tabs.iter().any(|t| foreign(t.id)))
                .filter(|g| later.window_groups.iter().any(|l| l.id == g.id))
                .map(|g| g.id)
                .collect();
            let order = |groups: &[crate::layout::WindowGroup]| -> Vec<GroupId> {
                groups
                    .iter()
                    .map(|g| g.id)
                    .filter(|id| others.contains(id))
                    .collect()
            };
            let earlier_order = order(&strip.window_groups);
            let later_order = order(&later.window_groups);
            for (index, group) in earlier_order.iter().enumerate() {
                if later_order.get(index) == Some(group) {
                    continue;
                }
                if let Some(group) = strip.window_groups.iter().find(|g| g.id == *group) {
                    for tab in group.tabs.iter().filter(|t| foreign(t.id)) {
                        note(tab.id, Act::Reorder);
                    }
                }
            }
        }
    }
    let live = |id: u64| now.tabs.contains_key(&id) || now.strips.contains_key(&StripId(id));
    let lived = |id: u64| was.tabs.contains_key(&id) || was.strips.contains_key(&StripId(id));
    let mut ids: Vec<u64> = before
        .authors
        .keys()
        .chain(after.authors.keys())
        .copied()
        .filter(|id| live(*id) && lived(*id))
        .collect();
    ids.compact_sort();
    ids.dedup();
    for id in ids {
        if foreign(id) && before.authors.get(&id) != after.authors.get(&id) {
            note(id, Act::Attribute);
        }
    }
    touched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Strip, Tab, TileContent, WindowGroup};
    use crate::width::WindowWidth;
    use crate::workspace::Workspace;

    const ADA: &str = "ada";
    const BEN: &str = "ben";

    fn tab(id: u64) -> Tab {
        Tab {
            id,
            title: format!("tab {id}"),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
        }
    }

    fn group(id: GroupId, tabs: &[u64]) -> WindowGroup {
        WindowGroup {
            id,
            tabs: tabs.iter().copied().map(tab).collect(),
            active_tab: tabs.first().copied().unwrap_or_default(),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// One workspace (1); strips 10 ("Lounge", nobody's) and 20 ("Ada's",
    /// Ada's); groups 100 (Ada's tab 1), 200 (Ben's tab 2), 300 (Ben's tab
    /// 3) in strip 10.
    fn desktop() -> SharedDesktop {
        let mut workspace = Workspace::new(WorkspaceId(1), "Lounge");
        let mut lounge = Strip::new(StripId(10), "Lounge");
        lounge.window_groups = vec![group(100, &[1]), group(200, &[2]), group(300, &[3])];
        workspace.strips = vec![lounge, Strip::new(StripId(20), "Ada's")];
        let tree = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };
        let authors = [(1, ADA), (2, BEN), (3, BEN), (20, ADA)]
            .into_iter()
            .map(|(id, who)| (id, who.to_owned()))
            .collect();
        SharedDesktop {
            tree,
            authors,
            records: Vec::new(),
        }
    }

    fn strip_mut(desktop: &mut SharedDesktop, id: u64) -> &mut Strip {
        desktop.tree.workspaces[0]
            .strips
            .iter_mut()
            .find(|s| s.id == StripId(id))
            .unwrap()
    }

    fn acts(after: &SharedDesktop, writer: &str) -> Vec<(u64, Act)> {
        foreign_acts(&desktop(), after, writer)
            .into_iter()
            .map(|t| (t.id, t.act))
            .collect()
    }

    #[test]
    fn closing_moving_and_reordering_ones_own_windows_touches_nobody_else() {
        let mut after = desktop();
        // Ada moves her window past both of Ben's, then into her own strip.
        let lounge = strip_mut(&mut after, 10);
        lounge.window_groups.rotate_left(1);
        assert!(acts(&after, ADA).is_empty());
        let mine = strip_mut(&mut after, 10).window_groups.remove(2);
        strip_mut(&mut after, 20).window_groups.push(mine);
        assert!(acts(&after, ADA).is_empty());
        // Then closes it, and opens another.
        strip_mut(&mut after, 20).window_groups.clear();
        strip_mut(&mut after, 20)
            .window_groups
            .push(group(400, &[4]));
        after.authors.insert(4, ADA.into());
        assert!(acts(&after, ADA).is_empty());
    }

    #[test]
    fn closing_someone_elses_window_is_named_with_its_owner() {
        let mut after = desktop();
        strip_mut(&mut after, 10).window_groups.remove(1);
        let touched = foreign_acts(&desktop(), &after, ADA);
        assert_eq!(
            touched,
            vec![Touched {
                id: 2,
                act: Act::Close,
                owner: Some(BEN.into()),
            }]
        );
        // Ben closing his own is his to do.
        assert!(acts(&after, BEN).is_empty());
    }

    #[test]
    fn moving_someone_elses_window_to_another_group_or_strip_is_a_move() {
        let mut into_group = desktop();
        let lounge = strip_mut(&mut into_group, 10);
        let bens = lounge.window_groups.remove(1).tabs.remove(0);
        lounge.window_groups[0].tabs.push(bens);
        assert_eq!(acts(&into_group, ADA), vec![(2, Act::Move)]);

        let mut into_strip = desktop();
        let bens = strip_mut(&mut into_strip, 10).window_groups.remove(2);
        strip_mut(&mut into_strip, 20).window_groups.push(bens);
        assert_eq!(acts(&into_strip, ADA), vec![(3, Act::Move)]);
    }

    #[test]
    fn swapping_two_of_someone_elses_windows_is_a_move() {
        let mut after = desktop();
        strip_mut(&mut after, 10).window_groups.swap(1, 2);
        assert_eq!(
            acts(&after, ADA),
            vec![(2, Act::Reorder), (3, Act::Reorder)]
        );
        assert!(acts(&after, BEN).is_empty());
    }

    #[test]
    fn a_strip_nobody_made_is_only_moderated() {
        let mut renamed = desktop();
        strip_mut(&mut renamed, 10).name = "Mine now".into();
        assert_eq!(acts(&renamed, ADA), vec![(10, Act::Rename)]);
        assert_eq!(acts(&renamed, BEN), vec![(10, Act::Rename)]);

        let mut removed = desktop();
        removed.tree.workspaces[0]
            .strips
            .retain(|s| s.id != StripId(10));
        // Removing it closes every window on it that was not Ada's too.
        assert_eq!(
            acts(&removed, ADA),
            vec![(10, Act::Close), (2, Act::Close), (3, Act::Close)]
        );
        // Ada's own strip is hers to remove and rename.
        let mut hers = desktop();
        strip_mut(&mut hers, 20).name = "Ada's corner".into();
        assert!(acts(&hers, ADA).is_empty());
    }

    #[test]
    fn taking_over_someone_elses_window_is_refused_as_an_attribution() {
        let mut claimed = desktop();
        claimed.authors.insert(2, ADA.into());
        assert_eq!(acts(&claimed, ADA), vec![(2, Act::Attribute)]);
        let mut unclaimed = desktop();
        unclaimed.authors.remove(&3);
        assert_eq!(acts(&unclaimed, ADA), vec![(3, Act::Attribute)]);
        // Claiming the room's strip is one too.
        let mut strip = desktop();
        strip.authors.insert(10, ADA.into());
        assert_eq!(acts(&strip, ADA), vec![(10, Act::Attribute)]);
        // Giving away one's own window is not.
        let mut given = desktop();
        given.authors.insert(1, BEN.into());
        assert!(acts(&given, ADA).is_empty());
    }

    #[test]
    fn acts_are_decided_on_the_items_moderation_node() {
        let touched = Touched {
            id: 2,
            act: Act::Close,
            owner: Some(BEN.into()),
        };
        let node = act_node("room-1", &touched);
        assert_eq!(
            node,
            TreePath::room("room-1")
                .app(DESKTOP_APP)
                .kind(DESKTOP_KIND)
                .resource(DESKTOP_RECORD)
                .item("2")
                .field(MODERATION_FIELD)
                .action("close")
        );
        // The roles' moderation rule covers it: Moderators may, Editors not.
        let moderation = tilo_action_policy::roles::moderation("room-1");
        assert!(moderation.covers(&node));
        // And the node a desktop asks about, which no one window's grant
        // covers.
        assert!(moderation.covers(&moderation_node("room-1")));
        assert!(!node.covers(&moderation_node("room-1")));
    }
}
