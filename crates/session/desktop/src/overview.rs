//! Plain-data projection for the workspace overview.
//!
//! The overview is the one desktop surface that deliberately reveals the
//! complete layout hierarchy: workspace -> strip -> window group -> tab.
//! It also attaches present collaborators to the tab they are looking at.
//! Permissions and the full sharing inventory belong to workspace settings;
//! keeping them out of this projection lets the overview remain a spatial
//! explanation instead of becoming a second settings screen.
//!
//! What only the host knows (who sits at this device, how its workspace
//! settings name what a share covers, an app's artwork) comes through
//! [`OverviewHost`].

use collab_types::caps::{Access, Effect, Principal, Scope, Subject};
use collab_types::role::Role as CapRole;
use tilo_desktop_model::{
    Collaborator, DesktopTree, GroupId, StripId, TabId, UserId, Workspace, WorkspaceSlot,
};
use tilo_sort::CompactSort as _;

use crate::i18n::{t, t_args};
use crate::window_switcher::{WindowApp, WindowSource};
use tilo_desktop_surface::names;

/// What the overview needs from its host beyond the desktop tree.
pub trait OverviewHost: WindowSource {
    /// The person at this device.
    fn local_user(&self) -> &str;

    /// What a capability covers, in the words the host's workspace settings use, so a share here is
    /// recognisable where it is revoked.
    fn scope_label(&self, scope: &Scope) -> String;

    /// Where in the workspace a grant applies, in the same words.
    fn node_label(&self, node: &tilo_action_policy::TreePath) -> String;
}

/// Cached, plain-data snapshot of a single workspace for rendering a card.
#[derive(Clone, Debug)]
pub struct CardCache {
    pub id: WorkspaceSlot,
    pub name: String,
    pub strip_count: usize,
}

/// The overview widget's logical state.
pub struct OverviewView {
    cards: Vec<CardCache>,
    active: Option<WorkspaceSlot>,
}

impl OverviewView {
    pub fn new() -> Self {
        Self {
            cards: Vec::new(),
            active: None,
        }
    }

    /// Rebuild the card cache from the desktop tree.
    pub fn sync(&mut self, tree: &DesktopTree) {
        self.cards = tree
            .workspaces
            .iter()
            .map(|w| CardCache {
                id: w.slot,
                name: w.name.clone(),
                strip_count: w.strips.len(),
            })
            .collect();
        self.active = Some(tree.active);
    }

    pub fn cards(&self) -> &[CardCache] {
        &self.cards
    }

    pub fn is_active(&self, idx: usize) -> bool {
        match (self.cards.get(idx), self.active) {
            (Some(card), Some(active)) => card.id == active,
            _ => false,
        }
    }
}

impl Default for OverviewView {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Workspaces: people, sharing, and where you are
// ---------------------------------------------------------------------------

/// `role` and `present` are kept apart on purpose: role is what the workspace's capabilities say
/// this person *may* do, present is whether they're here *now*. A member who has gone home is still
/// a member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspacePerson {
    pub id: UserId,
    /// Display name if this session has met them, else the principal itself — at least the string
    /// you would revoke.
    pub name: String,
    pub role: CapRole,
    pub present: bool,
    /// The person sitting at this machine.
    pub is_you: bool,
}

/// A capability naming a *resource* — a path, document, device — held by a person. Workspace
/// membership is [`WorkspaceCard::people`] instead, so "shared one file with Bob" can't collapse
/// into "Bob is a member".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceShare {
    /// In `workspace_settings::scope_label` 's own words, so a revoke there is recognisable here.
    pub what: String,
    pub with: Vec<String>,
    /// Whether any holder may write, not just read.
    pub writable: bool,
}

/// One tab nested inside its window group in the overview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewTab {
    pub id: TabId,
    pub title: String,
    pub is_active: bool,
    /// The application the tab shows, drawn by its mark.
    pub app: WindowApp,
    /// Present collaborators whose published focus points at this tab. The local person is
    /// deliberately absent: an untouched solo workspace should look like a desktop, not a
    /// collaboration dashboard.
    pub people: Vec<WorkspacePerson>,
}

/// One non-overlapping window group in a strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewGroup {
    pub id: GroupId,
    /// Persisted share of the strip's usable width. Rendering may compress all groups to fit, but
    /// retains these proportions.
    pub width_permille: u32,
    pub is_current: bool,
    pub tabs: Vec<OverviewTab>,
}

impl OverviewGroup {
    pub fn active_title(&self) -> String {
        self.tabs
            .iter()
            .find(|tab| tab.is_active)
            .or_else(|| self.tabs.first())
            .map_or_else(|| t("overview-empty-window"), |tab| tab.title.clone())
    }
}

/// One cheap, anonymous row of non-overlapping window groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewStrip {
    pub id: StripId,
    /// What the strip is called, as the strip switcher names it.
    pub name: String,
    pub is_current: bool,
    pub groups: Vec<OverviewGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceCard {
    pub id: WorkspaceSlot,
    pub name: String,
    pub is_current: bool,
    /// Owner first.
    pub people: Vec<WorkspacePerson>,
    /// Most-shared first.
    pub shares: Vec<WorkspaceShare>,
    /// Spatial contents, preserving tree order at every level.
    pub strips: Vec<OverviewStrip>,
    /// Where it runs and what it used, once hosting means something to the person
    /// ([`crate::overview_tilo::usage_line`]).
    pub hosting: Option<String>,
}

impl WorkspaceCard {
    pub fn present_count(&self) -> usize {
        self.people.iter().filter(|p| p.present).count()
    }

    pub fn others(&self) -> impl Iterator<Item = &WorkspacePerson> {
        self.people.iter().filter(|p| !p.is_you)
    }

    pub fn is_private(&self) -> bool {
        self.others().next().is_none()
    }

    pub fn group_count(&self) -> usize {
        self.strips.iter().map(|strip| strip.groups.len()).sum()
    }

    /// The applications open in the workspace, each once, in tree order: what a glance at its row
    /// should recognise it by.
    pub fn apps(&self) -> Vec<&WindowApp> {
        let mut apps: Vec<&WindowApp> = Vec::new();
        for tab in self
            .strips
            .iter()
            .flat_map(|strip| strip.groups.iter())
            .flat_map(|group| group.tabs.iter())
        {
            if !apps.iter().any(|app| app.key == tab.app.key) {
                apps.push(&tab.app);
            }
        }
        apps
    }
}

/// Falls back to the principal itself rather than a placeholder — a row labelled "Unknown" tells
/// the reader nothing, while the raw subject is both true and the value a revoke would name.
fn display_name(id: &UserId, ws: &Workspace, tree: &DesktopTree) -> String {
    known_collaborator(id, ws, tree).map_or_else(|| id.clone(), |c| c.name.clone())
}

fn known_collaborator<'a>(
    id: &UserId,
    ws: &'a Workspace,
    tree: &'a DesktopTree,
) -> Option<&'a Collaborator> {
    ws.collaborators
        .iter()
        .chain(tree.collaborators.iter())
        .find(|c| &c.id == id)
}

/// Everyone who may be in `ws` at unix time `now` , and whether they are here now.
pub fn workspace_people(
    ws: &Workspace,
    tree: &DesktopTree,
    host: &dyn OverviewHost,
    now: u64,
) -> Vec<WorkspacePerson> {
    let me = host.local_user();
    let _ = now;
    ws.roster()
        .into_iter()
        .map(|(id, role)| {
            // Being the local user isn't evidence of being in *this*
            // workspace — you're only in the one you're looking at. These are
            // two independent conditions (are-you-a-listed-collaborator,
            // are-you-the-local-user-in-the-active-workspace), not a mixed-up
            // field pairing, despite the superficial `a == b && c == d`
            // shape.
            #[allow(
                clippy::suspicious_operation_groupings,
                reason = "id == me and ws.id == tree.active are independent identity checks, not a swapped field pair"
            )]
            let present = ws.collaborators.iter().any(|c| c.id == id)
                || (id == me && ws.slot == tree.active);
            WorkspacePerson {
                name: display_name(&id, ws, tree),
                present,
                is_you: id == me,
                role,
                id,
            }
        })
        .collect()
}

/// Grouped by what is shared (scope, not capability): two grants on the same folder to two people
/// are one share with two holders.
pub fn workspace_shares(
    ws: &Workspace,
    tree: &DesktopTree,
    host: &dyn OverviewHost,
    now: u64,
) -> Vec<WorkspaceShare> {
    let mut out: Vec<WorkspaceShare> = Vec::new();
    let mut share = |what: String, holder: String, writable: bool| match out
        .iter_mut()
        .find(|s| s.what == what)
    {
        Some(existing) => {
            if !existing.with.contains(&holder) {
                existing.with.push(holder);
            }
            existing.writable |= writable;
        }
        None => out.push(WorkspaceShare {
            what,
            with: vec![holder],
            writable,
        }),
    };
    // What one person was given beyond their role in the workspace: a grant on a part of it, or a
    // file reached outside the sandbox's own.
    let node = ws.node();
    let cx = tilo_desktop_model::context_at(now);
    for grant in &ws.policy.grants {
        if grant.node == node
            || !grant.node.is_within(&node)
            || grant.answer != tilo_action_policy::Answer::Allow
            || !grant.is_live(&cx)
        {
            continue;
        }
        let Some(holder) = grant.grantee.person_id() else {
            continue;
        };
        share(
            host.node_label(&grant.node),
            display_name(&holder.to_owned(), ws, tree),
            grant
                .kinds
                .contains(tilo_action_policy::ActionKind::Undoable),
        );
    }
    for cap in &ws.sandbox {
        if !cap.is_live_at(Some(now)) || cap.effect != Effect::Grant {
            continue;
        }
        let holder = match &cap.subject {
            Subject::Principal(Principal::Person(p)) => p.clone(),
            // Resolving the durable name here stops one holder appearing twice on the same share.
            Subject::User(key) => match Principal::person_from_key(*key) {
                Principal::Person(p) => p,
                _ => continue,
            },
            Subject::Principal(_) => continue,
        };
        share(
            host.scope_label(&cap.scope),
            display_name(&holder, ws, tree),
            cap.access == Access::Write,
        );
    }
    out.compact_sort_by(|a, b| b.with.len().cmp(&a.with.len()).then(a.what.cmp(&b.what)));
    out
}

/// Every workspace as the overview shows it at unix time `now` .
pub fn workspace_cards(
    tree: &DesktopTree,
    host: &dyn OverviewHost,
    now: u64,
) -> Vec<WorkspaceCard> {
    tree.workspaces
        .iter()
        .map(|ws| WorkspaceCard {
            id: ws.slot,
            name: names::workspace(&ws.name),
            is_current: ws.slot == tree.active,
            people: workspace_people(ws, tree, host, now),
            shares: workspace_shares(ws, tree, host, now),
            strips: overview_strips(ws, tree, host, now),
            hosting: None,
        })
        .collect()
}

fn overview_strips(
    ws: &Workspace,
    tree: &DesktopTree,
    host: &dyn OverviewHost,
    now: u64,
) -> Vec<OverviewStrip> {
    let people = workspace_people(ws, tree, host, now);
    let active_strip = tree.active_strip.get(&ws.slot).copied();
    ws.strips
        .iter()
        .enumerate()
        .map(|(at, strip)| {
            let active_group = tree.active_group_for(strip.id);
            OverviewStrip {
                id: strip.id,
                name: names::strip_named(&strip.name, at),
                is_current: ws.slot == tree.active && active_strip == Some(strip.id),
                groups: strip
                    .window_groups
                    .iter()
                    .map(|group| OverviewGroup {
                        id: group.id,
                        width_permille: group.width.permille(),
                        is_current: ws.slot == tree.active
                            && active_strip == Some(strip.id)
                            && active_group == Some(group.id),
                        tabs: group
                            .tabs
                            .iter()
                            .map(|tab| OverviewTab {
                                id: tab.id,
                                title: if tab.title.trim().is_empty() {
                                    t("overview-untitled")
                                } else {
                                    tab.title.clone()
                                },
                                is_active: tab.id == group.active_tab,
                                app: WindowApp::of(&tab.tile, host),
                                people: people
                                    .iter()
                                    .filter(|person| {
                                        !person.is_you
                                            && person.present
                                            && ws
                                                .collaborators
                                                .iter()
                                                .find(|collaborator| collaborator.id == person.id)
                                                .is_some_and(|c| c.active_tab == Some(tab.id))
                                    })
                                    .cloned()
                                    .collect(),
                            })
                            .collect(),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// Names people while it helps, counts once it stops: "Shared with Ada, Bob, Cleo, Dara, Eve" is a
/// wall of text where "Shared with Ada and 4 others" is a fact.
pub fn workspace_access_label(card: &WorkspaceCard) -> String {
    let others: Vec<&WorkspacePerson> = card.others().collect();
    let name = |index: usize| others.get(index).map_or("", |p| p.name.as_str());
    match others.len() {
        // Nobody else in it: a workspace of the person's own, whether or not it recorded who
        // holds it.
        0 => t("overview-access-only-you"),
        1 => t_args("overview-access-one", &[("name", name(0).into())]),
        2 => t_args(
            "overview-access-two",
            &[("name", name(0).into()), ("other", name(1).into())],
        ),
        n => t_args(
            "overview-access-many",
            &[
                ("name", name(0).into()),
                ("count", n.saturating_sub(1).into()),
            ],
        ),
    }
}

/// `None` when it shares nothing — the caller draws nothing rather than a zero.
pub fn workspace_shares_label(card: &WorkspaceCard) -> Option<String> {
    match card.shares.len() {
        0 => None,
        n => Some(t_args("overview-shares", &[("count", n.into())])),
    }
}

/// `None` when nobody is here, including you — every workspace you aren't in.
pub fn workspace_presence_label(card: &WorkspaceCard) -> Option<String> {
    let live: Vec<&WorkspacePerson> = card.people.iter().filter(|p| p.present).collect();
    let others: Vec<&&WorkspacePerson> = live.iter().filter(|p| !p.is_you).collect();
    let name = || others.first().map_or("", |p| p.name.as_str());
    match (live.iter().any(|p| p.is_you), others.len()) {
        (false, 0) => None,
        (true, 0) => Some(t("overview-presence-you")),
        (true, 1) => Some(t_args(
            "overview-presence-you-and-one",
            &[("name", name().into())],
        )),
        (true, n) => Some(t_args(
            "overview-presence-you-and-many",
            &[("count", n.into())],
        )),
        (false, 1) => Some(t_args("overview-presence-one", &[("name", name().into())])),
        (false, n) => Some(t_args("overview-presence-many", &[("count", n.into())])),
    }
}

/// What began a press on the overview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressSource {
    /// The primary mouse button.
    Mouse,
    /// A direct-touch contact, by its id.
    Touch(u64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PressEnd {
    /// Released where it began: a click or a tap on `tab` 's preview, or on the panel when `tab` is
    /// `None` .
    Tap {
        tab: Option<TabId>,
        at: vello::kurbo::Point,
    },
    /// `tab` 's preview was dragged and let go at `at` .
    Drop { tab: TabId, at: vello::kurbo::Point },
    /// Dragged from somewhere with nothing to carry.
    Nothing,
}

/// One press on the overview, from down to up: a pointer button or a finger. It carries the window
/// whose preview it began on, and turns into a drag once it travels past the slop, so a tap and a
/// drag are told apart the same way for a mouse and for a finger.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverviewPress {
    pub source: PressSource,
    /// The window whose preview the press began on.
    pub tab: Option<TabId>,
    origin: vello::kurbo::Point,
    /// Where the pointer or finger is now.
    pub at: vello::kurbo::Point,
    dragging: bool,
}

impl OverviewPress {
    pub fn new(source: PressSource, tab: Option<TabId>, at: vello::kurbo::Point) -> Self {
        Self {
            source,
            tab,
            origin: at,
            at,
            dragging: false,
        }
    }

    /// The pointer or finger moved to `to` .
    pub fn moved(&mut self, to: vello::kurbo::Point) {
        self.at = to;
        let slop = match self.source {
            // The toolkit's own thresholds, so a preview starts dragging exactly when anything else
            // dragged would.
            PressSource::Mouse => ui::dnd::DRAG_START_THRESHOLD_PX,
            PressSource::Touch(_) => ui::gesture::TouchGestureArena::TAP_SLOP,
        };
        if self.origin.distance(to) > slop {
            self.dragging = true;
        }
    }

    /// A window preview is being carried.
    pub fn is_dragging(&self) -> bool {
        self.dragging && self.tab.is_some()
    }

    /// Let go at `at` .
    pub fn release(mut self, at: vello::kurbo::Point) -> PressEnd {
        self.moved(at);
        match (self.dragging, self.tab) {
            (false, tab) => PressEnd::Tap { tab, at },
            (true, Some(tab)) => PressEnd::Drop { tab, at },
            (true, None) => PressEnd::Nothing,
        }
    }
}

#[cfg(test)]
mod press_tests {
    use super::*;
    use vello::kurbo::Point;

    #[test]
    fn a_press_is_a_tap_until_it_travels_past_the_slop() {
        let start = Point::new(100.0, 100.0);
        let mut click = OverviewPress::new(PressSource::Mouse, Some(7), start);
        click.moved(Point::new(102.0, 100.0));
        assert!(!click.is_dragging());
        assert_eq!(
            click.release(Point::new(102.0, 101.0)),
            PressEnd::Tap {
                tab: Some(7),
                at: Point::new(102.0, 101.0)
            }
        );

        let mut drag = OverviewPress::new(PressSource::Mouse, Some(7), start);
        drag.moved(Point::new(140.0, 180.0));
        assert!(drag.is_dragging());
        drag.moved(start);
        assert!(drag.is_dragging(), "coming back does not undo a drag");
        assert_eq!(drag.release(start), PressEnd::Drop { tab: 7, at: start });
    }

    #[test]
    fn a_finger_gets_the_wider_touch_slop() {
        let start = Point::new(0.0, 0.0);
        let wobble = Point::new(10.0, 0.0);
        let mut finger = OverviewPress::new(PressSource::Touch(3), Some(7), start);
        finger.moved(wobble);
        assert!(!finger.is_dragging(), "a finger's wobble is still a tap");
        let mut mouse = OverviewPress::new(PressSource::Mouse, Some(7), start);
        mouse.moved(wobble);
        assert!(mouse.is_dragging());
    }

    #[test]
    fn a_drag_that_carries_nothing_does_nothing() {
        let start = Point::new(0.0, 0.0);
        let press = OverviewPress::new(PressSource::Touch(1), None, start);
        assert_eq!(press.release(Point::new(90.0, 0.0)), PressEnd::Nothing);
        let press = OverviewPress::new(PressSource::Touch(1), None, start);
        assert_eq!(
            press.release(start),
            PressEnd::Tap {
                tab: None,
                at: start
            }
        );
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;
    use collab_types::caps::{CapOverride, Expiry};
    use std::sync::atomic::{AtomicU64, Ordering};
    use tilo_desktop_model::{AppKind, Strip, Tab, TileContent, WindowGroup, WindowWidth};

    const ME: &str = "me";
    /// Well after the expired grants below, well before any real one ends.
    const NOW: u64 = 1_800_000_000;

    struct Host;

    impl WindowSource for Host {}

    impl OverviewHost for Host {
        fn local_user(&self) -> &str {
            ME
        }

        fn scope_label(&self, scope: &Scope) -> String {
            format!("{scope:?}")
        }

        fn node_label(&self, node: &tilo_action_policy::TreePath) -> String {
            node.to_string()
        }
    }

    fn workspace_cards(tree: &DesktopTree) -> Vec<WorkspaceCard> {
        super::workspace_cards(tree, &Host, NOW)
    }

    fn fresh_id() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    fn ws(name: &str) -> Workspace {
        let mut w = Workspace::new(WorkspaceSlot(fresh_id()), name);
        w.set_owner(ME.to_string());
        w
    }

    fn tree_with(workspaces: Vec<Workspace>) -> DesktopTree {
        let active = workspaces.first().map(|w| w.slot).unwrap_or_default();
        DesktopTree {
            workspaces,
            active,
            ..Default::default()
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn workspace_access_distinguishes_members_shares_and_expired_grants() {
        fn a_workspace_only_you_own_reads_as_private() {
            let tree = tree_with(vec![ws("Home")]);
            let cards = workspace_cards(&tree);
            assert_eq!(cards.len(), 1);
            assert!(cards[0].is_private());
            assert_eq!(workspace_access_label(&cards[0]), "Only you");
            assert_eq!(workspace_shares_label(&cards[0]), None);
        }

        fn a_member_makes_the_workspace_read_as_shared_with_them() {
            let mut w = ws("Design");
            w.admit("ada", collab_types::Role::Editor);
            w.collaborators.push(Collaborator::new("ada", "Ada"));
            let tree = tree_with(vec![w]);
            let card = &workspace_cards(&tree)[0];
            assert!(!card.is_private());
            assert_eq!(workspace_access_label(card), "Shared with Ada");
            // Membership is not a resource share — the two lists stay disjoint.
            assert_eq!(workspace_shares_label(card), None);
        }

        fn a_file_grant_is_a_share_and_not_a_membership() {
            let mut w = ws("Design");
            w.sandbox.push(CapOverride::grant(
                Subject::person("ada"),
                Scope::FsPath("/var/workspace/brief".to_string()),
                Access::Read,
            ));
            let tree = tree_with(vec![w]);
            let card = &workspace_cards(&tree)[0];
            // Ada holds a file, not the workspace: she is not a member…
            assert!(card.is_private(), "a file grant must not admit a member");
            assert_eq!(workspace_access_label(card), "Only you");
            // …and the file shows up as exactly one share, named the way the permissions pane names
            // it.
            assert_eq!(card.shares.len(), 1);
            assert_eq!(card.shares[0].with, vec!["ada".to_string()]);
            assert_eq!(
                workspace_shares_label(card).as_deref(),
                Some("1 item shared")
            );
        }

        fn two_grants_on_one_path_are_one_share_with_two_holders() {
            let mut w = ws("Design");
            for who in ["ada", "bob"] {
                w.sandbox.push(CapOverride::grant(
                    Subject::person(who),
                    Scope::FsPath("/var/workspace/brief".to_string()),
                    Access::Read,
                ));
            }
            let tree = tree_with(vec![w]);
            let card = &workspace_cards(&tree)[0];
            assert_eq!(card.shares.len(), 1, "one path is one share");
            assert_eq!(card.shares[0].with.len(), 2);
            assert_eq!(
                workspace_shares_label(card).as_deref(),
                Some("1 item shared")
            );
        }

        fn write_grants_are_marked_writable() {
            let mut w = ws("Design");
            w.sandbox.push(CapOverride::grant(
                Subject::person("ada"),
                Scope::FsPath("/var/workspace/brief".to_string()),
                Access::Write,
            ));
            let tree = tree_with(vec![w]);
            let card = &workspace_cards(&tree)[0];
            assert!(card.shares[0].writable);
        }

        fn expired_grants_neither_admit_members_nor_count_as_shares() {
            let mut w = ws("Design");
            let node = w.node();
            w.grant(
                tilo_action_policy::Grant::new(
                    "ada-for-a-while",
                    tilo_action_policy::Grantee::person("ada"),
                    node.app("so.tilo.notes"),
                    tilo_action_policy::KindSet::ALL,
                    tilo_action_policy::Answer::Allow,
                    tilo_action_policy::Grantee::person(ME),
                )
                .with_lifetime(tilo_action_policy::Lifetime::Until(1_000)),
            );
            w.sandbox.push(
                CapOverride::grant(
                    Subject::person("ada"),
                    Scope::FsPath("/var/workspace/brief".to_string()),
                    Access::Read,
                )
                .expiring(Expiry::AtUnix(1)),
            );
            let tree = tree_with(vec![w]);
            let card = &workspace_cards(&tree)[0];
            assert!(card.is_private(), "an expired membership admits nobody");
            assert!(card.shares.is_empty(), "an expired share is not shared");
        }

        fn access_label_counts_once_naming_stops_helping() {
            let mut w = ws("Design");
            for who in ["ada", "bob", "cleo", "dara"] {
                w.admit(who, collab_types::Role::Editor);
            }
            let tree = tree_with(vec![w]);
            let card = &workspace_cards(&tree)[0];
            assert_eq!(workspace_access_label(card), "Shared with ada and 3 others");
        }

        a_workspace_only_you_own_reads_as_private();
        a_member_makes_the_workspace_read_as_shared_with_them();
        a_file_grant_is_a_share_and_not_a_membership();
        two_grants_on_one_path_are_one_share_with_two_holders();
        write_grants_are_marked_writable();
        expired_grants_neither_admit_members_nor_count_as_shares();
        access_label_counts_once_naming_stops_helping();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn workspace_presence_is_who_is_here_now_and_is_attached_to_tabs() {
        fn presence_is_who_is_here_now_not_who_is_a_member() {
            let mut w = ws("Design");
            for who in ["ada", "bob"] {
                w.admit(who, collab_types::Role::Editor);
            }
            // Only Ada actually showed up.
            w.collaborators.push(Collaborator::new("ada", "Ada"));
            let tree = tree_with(vec![w]);
            let card = &workspace_cards(&tree)[0];
            assert_eq!(card.people.len(), 3, "you + two members");
            assert_eq!(card.present_count(), 2, "you and Ada, not Bob");
            assert_eq!(
                workspace_presence_label(card).as_deref(),
                Some("You and Ada are here")
            );
        }

        fn a_workspace_you_are_not_in_does_not_report_you_as_here() {
            let here = ws("Home");
            let elsewhere = ws("Design");
            let tree = tree_with(vec![here, elsewhere]);
            let cards = workspace_cards(&tree);
            assert_eq!(
                workspace_presence_label(&cards[0]).as_deref(),
                Some("You are here")
            );
            assert_eq!(
                workspace_presence_label(&cards[1]),
                None,
                "you cannot be in two workspaces at once"
            );
        }

        fn spatial_projection_preserves_strip_group_and_tab_nesting() {
            let mut w = ws("Design");
            let strip_id = StripId(fresh_id());
            let group_id = 41;
            let first_tab = 51;
            let second_tab = 52;
            w.strips = vec![Strip {
                id: strip_id,
                name: "ignored spatial label".to_string(),
                window_groups: vec![WindowGroup {
                    id: group_id,
                    tabs: vec![
                        Tab {
                            id: first_tab,
                            title: "Brief".to_string(),
                            tile: TileContent::Stub {
                                kind: AppKind::Other("Notes".to_string()),
                                label: "Brief".to_string(),
                            },
                            app_title: None,
                            view: String::new(),
                            pinned: false,
                        },
                        Tab {
                            id: second_tab,
                            title: "References".to_string(),
                            tile: TileContent::Stub {
                                kind: AppKind::Browser,
                                label: "References".to_string(),
                            },
                            app_title: None,
                            view: String::new(),
                            pinned: false,
                        },
                    ],
                    active_tab: second_tab,
                    width: WindowWidth::Wide,
                    maximized: false,
                    focus_history: Vec::new(),
                }],
            }];
            let mut tree = tree_with(vec![w]);
            let slot = tree.active;
            tree.active_strip.insert(slot, strip_id);
            tree.set_active_group(strip_id, group_id);

            let card = &workspace_cards(&tree)[0];
            assert_eq!(card.strips.len(), 1);
            assert!(card.strips[0].is_current);
            assert_eq!(card.strips[0].groups.len(), 1);
            let group = &card.strips[0].groups[0];
            assert!(group.is_current);
            assert_eq!(group.tabs.len(), 2);
            assert_eq!(group.active_title(), "References");
            assert_eq!(group.width_permille, WindowWidth::Wide.permille());
            let apps: Vec<_> = card.apps().iter().map(|app| app.kind).collect();
            assert_eq!(
                apps,
                [ui::widget::AppKind::Editor, ui::widget::AppKind::Browser],
                "the workspace row names each open app once, in tree order"
            );
        }

        fn collaborator_focus_is_attached_to_the_tab_not_the_workspace_header() {
            let mut w = ws("Design");
            let strip_id = StripId(fresh_id());
            let tab_id = 71;
            w.admit("ada", collab_types::Role::Editor);
            w.collaborators
                .push(Collaborator::with_active_tab("ada", "Ada", tab_id));
            w.strips = vec![Strip {
                id: strip_id,
                name: String::new(),
                window_groups: vec![WindowGroup {
                    id: 72,
                    tabs: vec![Tab {
                        id: tab_id,
                        title: "Brief".to_string(),
                        tile: TileContent::Stub {
                            kind: AppKind::Other("Notes".to_string()),
                            label: "Brief".to_string(),
                        },
                        app_title: None,
                        view: String::new(),
                        pinned: false,
                    }],
                    active_tab: tab_id,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                }],
            }];
            let tree = tree_with(vec![w]);

            let tab = &workspace_cards(&tree)[0].strips[0].groups[0].tabs[0];
            assert_eq!(
                tab.people
                    .iter()
                    .map(|person| person.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["Ada"]
            );
            assert!(tab.people.iter().all(|person| !person.is_you));
        }

        presence_is_who_is_here_now_not_who_is_a_member();
        a_workspace_you_are_not_in_does_not_report_you_as_here();
        spatial_projection_preserves_strip_group_and_tab_nesting();
        collaborator_focus_is_attached_to_the_tab_not_the_workspace_header();
    }
}
