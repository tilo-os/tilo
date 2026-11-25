//! Who may close, move or rename what is on a desktop a workspace shares.
//!
//! A shared desktop keeps, beside its tree, who made each strip and opened
//! each window (`authors`, by strip or tab id). People close, move and
//! rename what they made themselves as an ordinary edit of the desktop, and
//! a member of the workspace closes and moves any of its windows the same
//! way ([`shares_windows`]): a window on a shared desktop is everyone's there.
//! Doing it to what someone else made acts on other people's content, which
//! is moderation ([`tilo_action_policy::roles::MODERATION_FIELD`]): it is
//! decided on that strip's or window's `moderation` node, which the
//! Moderator and Owner roles allow and an Editor's denies, and which a workspace
//! can grant anyone ([`act_node`]). What nobody made (a strip the workspace
//! itself lists) is nobody's own, so only moderation changes it.
//!
//! What someone who has left the workspace made is released ([`released`]):
//! it is no longer anyone's own, and whoever writes there closes or moves
//! it as their own, so a window of someone gone never stays stuck on
//! everyone's desktop. Who is still in the workspace is its governance's member
//! list (`tilo_peer_state::members::people`).
//!
//! [`foreign_acts`] names what one desktop value did to another's strips
//! and windows that its writer did not make. A server decides every write of
//! a shared desktop with it, and a desktop asks [`crate::commands::policy::may_rearrange_window`] before it
//! offers to close or move a window, so the offer and the decision agree.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use tilo_sort::CompactSort as _;

use serde::{Deserialize, Serialize};
use tilo_action_policy::roles::MODERATION_FIELD;
use tilo_action_policy::{Level, TreePath};

use crate::ids::{GroupId, StripId, TabId, WorkspaceId};
use crate::tree::Layout;

/// The app a workspace's shared desktop is published under.
pub const DESKTOP_APP: &str = "so.tilo.browser-desktop";
/// The record of [`DESKTOP_APP`] holding a workspace's shared desktop.
pub const DESKTOP_DOCUMENT: &str = "desktop";
/// The resource kind of a shared desktop in the published tree.
pub const DESKTOP_KIND: &str = "tilo.desktop";

/// Who made each strip and opened each window of a shared desktop, by strip
/// or tab id, as person ids.
pub type Authors = BTreeMap<u64, String>;

/// The agent run that opened a window on a person's behalf: what the shell says
/// beside the window, so everyone sees whose agent put it there and in which
/// run. The window stays the person's ([`Authors`]): the run is who opened it,
/// not who may close it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct OpenedBy {
    /// The run's id, as History groups the run's changes (`agent-run:…`).
    pub run: String,
    /// The agent whose session it is: its id and its person. What it is drawn in, so one
    /// agent is one colour beside every window it opened, on its face and in its avatar,
    /// whichever of its sessions opened it.
    pub agent: tilo_action_policy::MemberId,
    /// What the run is called: the agent's name.
    pub label: String,
    /// What the agent called the step that opened the window ("Fill Jane Doe's
    /// visa application"): its own words, never the agent's or its session's
    /// name; empty when the agent titled no step.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

/// A shared desktop as a workspace holds it: the tree and who made what.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SharedDesktop {
    #[serde(deserialize_with = "Layout::deserialize_via_json")]
    pub tree: Layout,
    /// The record each window shows, by tab, for the windows bound to one.
    #[serde(default)]
    pub records: Vec<(TabId, String)>,
    /// The item each window was opened on (a conversation a send landed in), by tab.
    /// Kept apart from the tab's view, which is the view a place shows: a window is
    /// reopened on its item, and a place is not a window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<(TabId, String)>,
    #[serde(default)]
    pub authors: Authors,
    /// The agent run that opened each window an agent opened, by tab.
    /// Written once with the window and kept until the window closes: a
    /// writer that knows nothing of it leaves it as it is.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub opened_by: BTreeMap<TabId, OpenedBy>,
}

impl SharedDesktop {
    /// The shared desktop in the JSON text a workspace's desktop record holds.
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

/// The node `touched` is decided on in `workspace`:
/// `workspace › desktop app › desktop › desktop › item › moderation › act`.
#[must_use]
pub fn act_node(workspace: &str, touched: &Touched) -> TreePath {
    TreePath::workspace(workspace)
        .app(DESKTOP_APP)
        .kind(DESKTOP_KIND)
        .resource(DESKTOP_DOCUMENT)
        .item(touched.id.to_string())
        .field(MODERATION_FIELD)
        .action(touched.act.verb())
}

/// Every strip's and window's moderation on `workspace`'s shared desktop: what a
/// desktop asks the workspace whether its person may do before it offers to
/// close or move what others made. A grant on one window alone does not
/// cover it; the workspace's moderators' does.
#[must_use]
pub fn moderation_node(workspace: &str) -> TreePath {
    TreePath::workspace(workspace)
        .app(DESKTOP_APP)
        .kind(DESKTOP_KIND)
        .resource(DESKTOP_DOCUMENT)
        .any(Level::Item)
        .field(MODERATION_FIELD)
        .any(Level::Action)
}

/// Whether `person` made the strip or window `id`.
#[must_use]
pub fn made_by(authors: &Authors, id: u64, person: &str) -> bool {
    authors.get(&id).is_some_and(|author| author == person)
}

/// Whether the strip or window `id` is released: someone made it who is no
/// longer among the workspace's `members`. `None` while the members are not
/// known: then nothing is released, so nobody's window is taken early.
#[must_use]
pub fn released(authors: &Authors, id: u64, members: Option<&BTreeSet<String>>) -> bool {
    members.is_some_and(|members| {
        authors
            .get(&id)
            .is_some_and(|author| !members.contains(author))
    })
}

/// Whether `person` is among the workspace's `members`, who share its windows: each of them
/// closes, moves and reorders any window there, whoever opened it. `None` while the members are
/// not known: then nobody shares what someone else opened yet.
#[must_use]
pub fn shares_windows(person: &str, members: Option<&BTreeSet<String>>) -> bool {
    members.is_some_and(|members| members.contains(person))
}

/// Where a window is: its workspace, strip and group.
type Place = (WorkspaceId, StripId, GroupId);

struct Index {
    tabs: HashMap<TabId, Place>,
    strips: HashMap<StripId, String>,
}

fn index(tree: &Layout) -> Index {
    let mut tabs = HashMap::new();
    let mut strips = HashMap::new();
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            strips.insert(strip.id, strip.name.clone());
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    tabs.insert(tab.id, (workspace.id(), strip.id, group.id));
                }
            }
        }
    }
    Index { tabs, strips }
}

/// What `after` did to the strips and windows of `before` that `writer` did
/// not make and that are not [`released`] (`members`: the workspace's current
/// members, when known), each once, in the order `before` holds them: windows closed or
/// moved, strips removed or renamed, and anything whose maker changed. A
/// window `writer` opened, or one opened since `before`, is theirs to
/// arrange; so is the order of their own windows among others', since a
/// state cannot tell moving one's own window past another from the reverse.
/// Every window is a member's to close, move and reorder ([`shares_windows`]),
/// so for a member only strips and who made what count.
#[must_use]
pub fn foreign_acts(
    before: &SharedDesktop,
    after: &SharedDesktop,
    writer: &str,
    members: Option<&BTreeSet<String>>,
) -> Vec<Touched> {
    let foreign =
        |id: u64| !made_by(&before.authors, id, writer) && !released(&before.authors, id, members);
    let shares = shares_windows(writer, members);
    let foreign_window = |id: u64| !shares && foreign(id);
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
            strip_acts(
                strip,
                &was,
                &now,
                after,
                (&foreign, &foreign_window),
                &mut note,
            );
        }
        let later = after
            .tree
            .workspaces
            .iter()
            .find(|w| w.id() == workspace.id());
        for tab in moved_on_canvas(workspace, later, &foreign_window) {
            note(tab, Act::Move);
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

/// The windows of `after` whose own state ([`crate::window_state`]) differs from what `before`
/// held, by tab (`was`, `now`: each window's sealed state as the record keeps it), that `writer`
/// did not make and that are not [`released`]. A window's state is its person's: unlike its
/// place, no member and no moderator changes it, so every one of these is refused.
#[must_use]
pub fn restated_windows(
    (before, was): (&SharedDesktop, &BTreeMap<TabId, String>),
    (after, now): (&SharedDesktop, &BTreeMap<TabId, String>),
    writer: &str,
    members: Option<&BTreeSet<String>>,
) -> Vec<TabId> {
    let live = index(&after.tree).tabs;
    let mut tabs: Vec<TabId> = was
        .keys()
        .chain(now.keys())
        .copied()
        .filter(|tab| live.contains_key(tab) && was.get(tab) != now.get(tab))
        .collect();
    tabs.compact_sort();
    tabs.dedup();
    // A window opened in this write is its author's as the write names them ([`claimed`]).
    let authors = |tab: TabId| before.authors.get(&tab).or_else(|| after.authors.get(&tab));
    tabs.retain(|tab| {
        authors(*tab).is_none_or(|author| author != writer)
            && !released(&before.authors, *tab, members)
    });
    tabs
}

/// What `after` did to `strip`, when the first of `foreign` names it, and to the windows in it
/// that the second names: the strip closed or renamed, its windows closed or moved, and other
/// people's windows reordered among themselves.
fn strip_acts(
    strip: &crate::layout::Strip,
    was: &Index,
    now: &Index,
    after: &SharedDesktop,
    (foreign_strip, foreign): (&dyn Fn(u64) -> bool, &dyn Fn(u64) -> bool),
    note: &mut dyn FnMut(u64, Act),
) {
    if foreign_strip(strip.id.0) {
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
        return;
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

/// The windows of `before` that `foreign` names whose group stands elsewhere
/// on the canvas in `after`: moving a window on the canvas moves it as much
/// as moving it along a strip. Placing one that stood nowhere yet (a canvas
/// finding room for a window someone opened) moves nothing.
fn moved_on_canvas(
    before: &crate::tree::Workspace,
    after: Option<&crate::tree::Workspace>,
    foreign: &dyn Fn(u64) -> bool,
) -> Vec<TabId> {
    before
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .filter(|group| {
            before.layout.place_of(group.id).is_some_and(|was_at| {
                after
                    .and_then(|w| w.layout.place_of(group.id))
                    .is_some_and(|now_at| now_at != was_at)
            })
        })
        .flat_map(|group| group.tabs.iter().map(|tab| tab.id))
        .filter(|tab| foreign(*tab))
        .collect()
}

/// The strips and windows `after` adds to `before` in someone else's name
/// than `writer`'s: claiming a window for another person makes it theirs to
/// keep, so it is an attribution of what they did not make.
#[must_use]
pub fn claimed(before: &SharedDesktop, after: &SharedDesktop, writer: &str) -> Vec<Touched> {
    let held = index(&before.tree);
    after
        .authors
        .iter()
        .filter(|(id, author)| {
            !held.tabs.contains_key(*id)
                && !held.strips.contains_key(&StripId(**id))
                && author.as_str() != writer
        })
        .map(|(id, author)| Touched {
            id: *id,
            act: Act::Attribute,
            owner: Some(author.clone()),
        })
        .collect()
}

/// The windows of `before` still in `after` that `after` binds to another
/// record than the one they showed: a record route is a window's identity,
/// so nobody rebinds it, not even its maker.
#[must_use]
pub fn rebound_windows(before: &SharedDesktop, after: &SharedDesktop) -> Vec<TabId> {
    let shown = index(&after.tree);
    let now: HashMap<TabId, &String> = after.records.iter().map(|(t, r)| (*t, r)).collect();
    let mut rebound: Vec<TabId> = before
        .records
        .iter()
        .filter(|(tab, record)| shown.tabs.contains_key(tab) && now.get(tab) != Some(&record))
        .map(|(tab, _)| *tab)
        .collect();
    rebound.compact_sort();
    rebound
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Strip, Tab, TileContent, WindowGroup};
    use crate::tree::Workspace;
    use crate::width::WindowWidth;

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
            app_title: None,
            view: String::new(),
            pinned: false,
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
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Lounge");
        let mut lounge = Strip::new(StripId(10), "Lounge");
        lounge.window_groups = vec![group(100, &[1]), group(200, &[2]), group(300, &[3])];
        workspace.strips = vec![lounge, Strip::new(StripId(20), "Ada's")];
        let tree = Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Layout::default()
        };
        let authors = [(1, ADA), (2, BEN), (3, BEN), (20, ADA)]
            .into_iter()
            .map(|(id, who)| (id, who.to_owned()))
            .collect();
        SharedDesktop {
            tree,
            authors,
            records: Vec::new(),
            items: Vec::new(),
            opened_by: std::collections::BTreeMap::new(),
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
        foreign_acts(&desktop(), after, writer, None)
            .into_iter()
            .map(|t| (t.id, t.act))
            .collect()
    }

    tilo_test_env::scenario! {
        fn whose_windows_and_strips_an_edit_acts_on;

        phase what_someone_who_left_made_is_released;
        // Ben has left the workspace: closing his window is no act on anyone's,
        // while the strip nobody made stays the workspace's.
        {
            let mut after = desktop();
            strip_mut(&mut after, 10)
                .window_groups
                .retain(|g| g.id != 200);
            let members: BTreeSet<String> = [ADA.to_owned()].into();
            assert_eq!(acts(&after, ADA), vec![(2, Act::Close)], "Ben still counts");
            assert!(
                foreign_acts(&desktop(), &after, ADA, Some(&members)).is_empty(),
                "Ben left: his window is released"
            );
            let mut removed = desktop();
            removed.tree.workspaces[0]
                .strips
                .retain(|s| s.id != StripId(10));
            assert_eq!(
                foreign_acts(&desktop(), &removed, ADA, Some(&members))
                    .into_iter()
                    .map(|t| (t.id, t.act))
                    .collect::<Vec<_>>(),
                vec![(10, Act::Close)],
                "the workspace's own strip is nobody's to release"
            );
        }

        phase a_member_closes_moves_and_reorders_anyones_window_but_not_their_strips;
        // Ada and Ben are both members: the workspace's windows are each of theirs to close, move
        // and reorder, while its own strip and who made what stay as they were.
        {
            let members: BTreeSet<String> = [ADA.to_owned(), BEN.to_owned()].into();
            let by_member = |after: &SharedDesktop| {
                foreign_acts(&desktop(), after, ADA, Some(&members))
                    .into_iter()
                    .map(|t| (t.id, t.act))
                    .collect::<Vec<_>>()
            };
            let mut closed = desktop();
            strip_mut(&mut closed, 10).window_groups.remove(1);
            assert!(by_member(&closed).is_empty(), "closing Ben's window");
            let mut moved = desktop();
            let bens = strip_mut(&mut moved, 10).window_groups.remove(2);
            strip_mut(&mut moved, 20).window_groups.push(bens);
            assert!(by_member(&moved).is_empty(), "moving Ben's window");
            let mut swapped = desktop();
            strip_mut(&mut swapped, 10).window_groups.swap(1, 2);
            assert!(by_member(&swapped).is_empty(), "reordering Ben's windows");
            let mut removed = desktop();
            removed.tree.workspaces[0]
                .strips
                .retain(|s| s.id != StripId(10));
            assert_eq!(by_member(&removed), vec![(10, Act::Close)], "the workspace's strip");
            let mut claimed = desktop();
            claimed.authors.insert(2, ADA.into());
            assert_eq!(by_member(&claimed), vec![(2, Act::Attribute)], "taking Ben's window");
            let visitor: BTreeSet<String> = [BEN.to_owned()].into();
            assert_eq!(
                foreign_acts(&desktop(), &closed, ADA, Some(&visitor))
                    .into_iter()
                    .map(|t| (t.id, t.act))
                    .collect::<Vec<_>>(),
                vec![(2, Act::Close)],
                "someone who is not a member closes only their own"
            );
        }

        phase closing_moving_and_reordering_ones_own_windows_touches_nobody_else;
        {
            let mut after = desktop();
            let lounge = strip_mut(&mut after, 10);
            lounge.window_groups.rotate_left(1);
            assert!(acts(&after, ADA).is_empty());
            let mine = strip_mut(&mut after, 10).window_groups.remove(2);
            strip_mut(&mut after, 20).window_groups.push(mine);
            assert!(acts(&after, ADA).is_empty());
            strip_mut(&mut after, 20).window_groups.clear();
            strip_mut(&mut after, 20)
                .window_groups
                .push(group(400, &[4]));
            after.authors.insert(4, ADA.into());
            assert!(acts(&after, ADA).is_empty());
        }

        phase closing_someone_elses_window_is_named_with_its_owner;
        {
            let mut after = desktop();
            strip_mut(&mut after, 10).window_groups.remove(1);
            let touched = foreign_acts(&desktop(), &after, ADA, None);
            assert_eq!(
                touched,
                vec![Touched {
                    id: 2,
                    act: Act::Close,
                    owner: Some(BEN.into()),
                }]
            );
            assert!(acts(&after, BEN).is_empty());
        }

        phase moving_someone_elses_window_to_another_group_or_strip_is_a_move;
        {
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

        phase swapping_two_of_someone_elses_windows_is_a_move;
        {
            let mut after = desktop();
            strip_mut(&mut after, 10).window_groups.swap(1, 2);
            assert_eq!(
                acts(&after, ADA),
                vec![(2, Act::Reorder), (3, Act::Reorder)]
            );
            assert!(acts(&after, BEN).is_empty());
        }

        phase a_strip_nobody_made_is_only_moderated;
        {
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
            let mut hers = desktop();
            strip_mut(&mut hers, 20).name = "Ada's corner".into();
            assert!(acts(&hers, ADA).is_empty());
        }

        phase taking_over_someone_elses_window_is_refused_as_an_attribution;
        {
            let mut claimed = desktop();
            claimed.authors.insert(2, ADA.into());
            assert_eq!(acts(&claimed, ADA), vec![(2, Act::Attribute)]);
            let mut unclaimed = desktop();
            unclaimed.authors.remove(&3);
            assert_eq!(acts(&unclaimed, ADA), vec![(3, Act::Attribute)]);
            let mut strip = desktop();
            strip.authors.insert(10, ADA.into());
            assert_eq!(acts(&strip, ADA), vec![(10, Act::Attribute)]);
            // Giving away one's own window is not.
            let mut given = desktop();
            given.authors.insert(1, BEN.into());
            assert!(acts(&given, ADA).is_empty());
        }

        phase acts_are_decided_on_the_items_moderation_node;
        {
            let touched = Touched {
                id: 2,
                act: Act::Close,
                owner: Some(BEN.into()),
            };
            let node = act_node("workspace-1", &touched);
            assert_eq!(
                node,
                TreePath::workspace("workspace-1")
                    .app(DESKTOP_APP)
                    .kind(DESKTOP_KIND)
                    .resource(DESKTOP_DOCUMENT)
                    .item("2")
                    .field(MODERATION_FIELD)
                    .action("close")
            );
            // The roles' moderation rule covers it: Moderators may, Editors not.
            let moderation = tilo_action_policy::roles::moderation("workspace-1");
            assert!(moderation.covers(&node));
            // And the node a desktop asks about, which no one window's grant
            // covers.
            assert!(moderation.covers(&moderation_node("workspace-1")));
            assert!(!node.covers(&moderation_node("workspace-1")));
        }
    }
}
