//! Plain-data projection for the workspace overview.
//!
//! The overview is the one shell surface that deliberately reveals the
//! complete layout hierarchy: workspace -> strip -> window group -> tab.
//! It also attaches present collaborators to the tab they are looking at.
//! Permissions and the full sharing inventory belong to workspace settings;
//! keeping them out of this projection lets the overview remain a spatial
//! explanation instead of becoming a second settings screen.

use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{
    CapRole, Collaborator, Effect, GroupId, Principal, StripId, Subject, TabId, UserId, Workspace,
    WorkspaceId, WorkspaceTree, local_user_id,
};

/// Cached, plain-data snapshot of a single workspace for rendering a card.
#[derive(Clone, Debug)]
pub struct CardCache {
    pub id: WorkspaceId,
    pub name: String,
    pub strip_count: usize,
}

/// The overview widget's logical state.
pub struct OverviewView {
    cards: Vec<CardCache>,
    active: Option<WorkspaceId>,
}

impl OverviewView {
    pub fn new() -> Self {
        Self {
            cards: Vec::new(),
            active: None,
        }
    }

    /// Rebuild the card cache from the authoritative `WorkspaceTree`.
    pub fn sync(&mut self, tree: &WorkspaceTree) {
        self.cards = tree
            .desktop
            .workspaces
            .iter()
            .map(|w| CardCache {
                id: w.id,
                name: w.name.clone(),
                strip_count: w.strips.len(),
            })
            .collect();
        self.active = Some(tree.desktop.active);
    }

    pub fn card_count(&self) -> usize {
        self.cards.len()
    }

    pub fn cards(&self) -> &[CardCache] {
        &self.cards
    }

    /// Out-of-range indices are ignored.
    pub fn click_card(&mut self, idx: usize, tree: &mut WorkspaceTree) {
        if let Some(card) = self.cards.get(idx) {
            tree.set_active(card.id);
            self.active = Some(tree.desktop.active);
        }
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
// Rooms: people, sharing, and where you are
// ---------------------------------------------------------------------------

/// `role` and `present` are kept apart on purpose: role is what the
/// room's capabilities say this person *may* do, present is whether
/// they're here *now*. A member who has gone home is still a member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomPerson {
    pub id: UserId,
    /// Display name if this session has met them, else the principal
    /// itself — at least the string you would revoke.
    pub name: String,
    pub role: CapRole,
    pub present: bool,
    /// The person sitting at this machine.
    pub is_you: bool,
}

/// A capability naming a *resource* — a path, document, device — held
/// by a person. Room membership is [`RoomCard::people`] instead, so
/// "shared one file with Bob" can't collapse into "Bob is a member".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomShare {
    /// In `workspace_settings::scope_label`'s own words, so a revoke
    /// there is recognisable here.
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
    pub app: crate::shell::window_switcher::WindowApp,
    /// Present collaborators whose published focus points at this tab.
    /// The local person is deliberately absent: an untouched solo workspace
    /// should look like a desktop, not a collaboration dashboard.
    pub people: Vec<RoomPerson>,
}

/// One non-overlapping window group in a strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewGroup {
    pub id: GroupId,
    /// Persisted share of the strip's usable width. Rendering may compress
    /// all groups to fit, but retains these proportions.
    pub width_permille: u16,
    pub is_current: bool,
    pub tabs: Vec<OverviewTab>,
}

impl OverviewGroup {
    pub fn active_title(&self) -> String {
        self.tabs
            .iter()
            .find(|tab| tab.is_active)
            .or_else(|| self.tabs.first())
            .map_or_else(
                || crate::shell::i18n::t("overview-empty-window"),
                |tab| tab.title.clone(),
            )
    }

    pub fn present_people(&self) -> impl Iterator<Item = &RoomPerson> {
        self.tabs.iter().flat_map(|tab| tab.people.iter())
    }
}

/// One cheap, anonymous row of non-overlapping window groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewStrip {
    pub id: StripId,
    pub is_current: bool,
    pub groups: Vec<OverviewGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomCard {
    pub id: WorkspaceId,
    pub name: String,
    pub is_current: bool,
    /// Owner first.
    pub people: Vec<RoomPerson>,
    /// Most-shared first.
    pub shares: Vec<RoomShare>,
    /// Spatial contents, preserving tree order at every level.
    pub strips: Vec<OverviewStrip>,
}

impl RoomCard {
    pub fn present_count(&self) -> usize {
        self.people.iter().filter(|p| p.present).count()
    }

    pub fn others(&self) -> impl Iterator<Item = &RoomPerson> {
        self.people.iter().filter(|p| !p.is_you)
    }

    pub fn is_private(&self) -> bool {
        self.others().next().is_none()
    }

    pub fn group_count(&self) -> usize {
        self.strips.iter().map(|strip| strip.groups.len()).sum()
    }

    /// The applications open in the room, each once, in tree order: what a
    /// glance at its row should recognise it by.
    pub fn apps(&self) -> Vec<&crate::shell::window_switcher::WindowApp> {
        let mut apps: Vec<&crate::shell::window_switcher::WindowApp> = Vec::new();
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

/// Falls back to the principal itself rather than a placeholder — a
/// row labelled "Unknown" tells the reader nothing, while the raw
/// subject is both true and the value a revoke would name.
fn display_name(id: &UserId, ws: &Workspace, tree: &WorkspaceTree) -> String {
    known_collaborator(id, ws, tree).map_or_else(|| id.clone(), |c| c.name.clone())
}

fn known_collaborator<'a>(
    id: &UserId,
    ws: &'a Workspace,
    tree: &'a WorkspaceTree,
) -> Option<&'a Collaborator> {
    ws.collaborators
        .iter()
        .chain(tree.desktop.collaborators.iter())
        .find(|c| &c.id == id)
}

pub fn room_people(ws: &Workspace, tree: &WorkspaceTree) -> Vec<RoomPerson> {
    room_people_with_local_user(ws, tree, &local_user_id())
}

fn room_people_with_local_user(ws: &Workspace, tree: &WorkspaceTree, me: &str) -> Vec<RoomPerson> {
    ws.roster()
        .into_iter()
        .map(|(id, role)| {
            // Being the local user isn't evidence of being in *this*
            // room — you're only in the one you're looking at. These are
            // two independent conditions (are-you-a-listed-collaborator,
            // are-you-the-local-user-in-the-active-room), not a mixed-up
            // field pairing, despite the superficial `a == b && c == d`
            // shape.
            #[allow(
                clippy::suspicious_operation_groupings,
                reason = "id == me and ws.id == tree.active are independent identity checks, not a swapped field pair"
            )]
            let present = ws.collaborators.iter().any(|c| c.id == id)
                || (id == me && ws.id == tree.desktop.active);
            RoomPerson {
                name: display_name(&id, ws, tree),
                present,
                is_you: id == me,
                role,
                id,
            }
        })
        .collect()
}

/// Grouped by what is shared (scope, not capability): two grants on
/// the same folder to two people are one share with two holders.
pub fn room_shares(ws: &Workspace, tree: &WorkspaceTree) -> Vec<RoomShare> {
    use crate::shell::workspace_settings::scope_label;
    let now = Some(tilo_clock::unix_now());
    let mut out: Vec<RoomShare> = Vec::new();
    for cap in &ws.capabilities {
        if !cap.is_live_at(now) || cap.effect != Effect::Grant {
            continue;
        }
        if cap.is_room_membership() || !cap.subject_is_person() {
            continue;
        }
        let holder = match &cap.subject {
            Subject::Principal(Principal::Person(p)) => p.clone(),
            // Resolving the durable name here stops one holder
            // appearing twice on the same share.
            Subject::User(key) => match Principal::person_from_key(*key) {
                Principal::Person(p) => p,
                _ => continue,
            },
            _ => continue,
        };
        let holder = display_name(&holder, ws, tree);
        let what = scope_label(&cap.scope);
        let writable = cap.verb.implies(crate::shell::state::Verb::Write);
        match out.iter_mut().find(|s| s.what == what) {
            Some(existing) => {
                if !existing.with.contains(&holder) {
                    existing.with.push(holder);
                }
                existing.writable |= writable;
            }
            None => out.push(RoomShare {
                what,
                with: vec![holder],
                writable,
            }),
        }
    }
    out.sort_by(|a, b| b.with.len().cmp(&a.with.len()).then(a.what.cmp(&b.what)));
    out
}

pub fn room_cards(tree: &WorkspaceTree) -> Vec<RoomCard> {
    room_cards_with_local_user(tree, &local_user_id())
}

pub(crate) fn room_cards_with_local_user(tree: &WorkspaceTree, local: &str) -> Vec<RoomCard> {
    tree.desktop
        .workspaces
        .iter()
        .map(|ws| RoomCard {
            id: ws.id,
            name: ws.name.clone(),
            is_current: ws.id == tree.desktop.active,
            people: room_people_with_local_user(ws, tree, local),
            shares: room_shares(ws, tree),
            strips: overview_strips(ws, tree, local),
        })
        .collect()
}

fn overview_strips(ws: &Workspace, tree: &WorkspaceTree, local: &str) -> Vec<OverviewStrip> {
    let people = room_people_with_local_user(ws, tree, local);
    let active_strip = tree.desktop.active_strip.get(&ws.id).copied();
    ws.strips
        .iter()
        .map(|strip| {
            let active_group = tree.desktop.active_group_for(strip.id);
            OverviewStrip {
                id: strip.id,
                is_current: ws.id == tree.desktop.active && active_strip == Some(strip.id),
                groups: strip
                    .window_groups
                    .iter()
                    .map(|group| OverviewGroup {
                        id: group.id,
                        width_permille: group.width.permille(),
                        is_current: ws.id == tree.desktop.active
                            && active_strip == Some(strip.id)
                            && active_group == Some(group.id),
                        tabs: group
                            .tabs
                            .iter()
                            .map(|tab| OverviewTab {
                                id: tab.id,
                                title: if tab.title.trim().is_empty() {
                                    crate::shell::i18n::t("window-switcher-untitled")
                                } else {
                                    tab.title.clone()
                                },
                                is_active: tab.id == group.active_tab,
                                app: crate::shell::window_switcher::WindowApp::of(&tab.tile),
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

/// Names people while it helps, counts once it stops: "Shared with
/// Ada, Bob, Cleo, Dara, Eve" is a wall of text where "Shared with Ada
/// and 4 others" is a fact.
pub fn room_access_label(card: &RoomCard) -> String {
    let others: Vec<&RoomPerson> = card.others().collect();
    match others.len() {
        0 => {
            if card.people.is_empty() {
                // No owner, no membership capability recorded at all.
                crate::shell::i18n::t("overview-access-nobody")
            } else {
                crate::shell::i18n::t("overview-access-private")
            }
        }
        1 => crate::shell::i18n::t_args(
            "overview-access-shared-one",
            &[(
                "name",
                others.first().map_or("", |p| p.name.as_str()).into(),
            )],
        ),
        2 => crate::shell::i18n::t_args(
            "overview-access-shared-two",
            &[
                (
                    "first",
                    others.first().map_or("", |p| p.name.as_str()).into(),
                ),
                (
                    "second",
                    others.get(1).map_or("", |p| p.name.as_str()).into(),
                ),
            ],
        ),
        n => crate::shell::i18n::t_args(
            "overview-access-shared-many",
            &[
                (
                    "name",
                    others.first().map_or("", |p| p.name.as_str()).into(),
                ),
                ("count", n.saturating_sub(1).into()),
            ],
        ),
    }
}

/// `None` when it shares nothing — the caller draws nothing rather than a zero.
pub fn room_shares_label(card: &RoomCard) -> Option<String> {
    match card.shares.len() {
        0 => None,
        n => Some(crate::shell::i18n::t_args(
            "overview-items-shared",
            &[("count", n.into())],
        )),
    }
}

/// `None` when nobody is here, including you — every room you aren't in.
pub fn room_presence_label(card: &RoomCard) -> Option<String> {
    let live: Vec<&RoomPerson> = card.people.iter().filter(|p| p.present).collect();
    let others: Vec<&&RoomPerson> = live.iter().filter(|p| !p.is_you).collect();
    match (live.iter().any(|p| p.is_you), others.len()) {
        (false, 0) => None,
        (true, 0) => Some(crate::shell::i18n::t("overview-presence-you")),
        (true, 1) => Some(crate::shell::i18n::t_args(
            "overview-presence-you-and-one",
            &[(
                "name",
                others.first().map_or("", |p| p.name.as_str()).into(),
            )],
        )),
        (true, n) => Some(crate::shell::i18n::t_args(
            "overview-presence-you-and-many",
            &[("count", n.into())],
        )),
        (false, 1) => Some(crate::shell::i18n::t_args(
            "overview-presence-one",
            &[(
                "name",
                others.first().map_or("", |p| p.name.as_str()).into(),
            )],
        )),
        (false, n) => Some(crate::shell::i18n::t_args(
            "overview-presence-many",
            &[("count", n.into())],
        )),
    }
}

/// Collaboration-only summary for the spatial overview. A private workspace
/// returns `None`, so the solo state does not announce the local person back
/// to themselves.
pub fn overview_people_label(card: &RoomCard) -> Option<String> {
    let present: Vec<&RoomPerson> = card.others().filter(|person| person.present).collect();
    match present.len() {
        0 if card.is_private() => None,
        0 => Some(room_access_label(card)),
        1 => Some(crate::shell::i18n::t_args(
            "overview-presence-one",
            &[(
                "name",
                present
                    .first()
                    .map_or("", |person| person.name.as_str())
                    .into(),
            )],
        )),
        n => Some(crate::shell::i18n::t_args(
            "overview-collaborators-here",
            &[("count", n.into())],
        )),
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

/// How a press on the overview ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PressEnd {
    /// Released where it began: a click or a tap on `tab`'s preview, or on
    /// the panel when `tab` is `None`.
    Tap {
        tab: Option<TabId>,
        at: vello::kurbo::Point,
    },
    /// `tab`'s preview was dragged and let go at `at`.
    Drop { tab: TabId, at: vello::kurbo::Point },
    /// Dragged from somewhere with nothing to carry.
    Nothing,
}

/// One press on the overview, from down to up: a pointer button or a
/// finger. It carries the window whose preview it began on, and turns
/// into a drag once it travels past the slop, so a tap and a drag are told
/// apart the same way for a mouse and for a finger.
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

    /// The pointer or finger moved to `to`.
    pub fn moved(&mut self, to: vello::kurbo::Point) {
        self.at = to;
        let slop = match self.source {
            // The toolkit's own thresholds, so a preview starts dragging
            // exactly when anything else dragged would.
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

    /// Let go at `at`.
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
mod room_tests {
    use super::*;
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{
        AppKind, CapOverride, Scope, Strip, Tab, TileContent, Verb, WindowGroup, WindowWidth,
    };

    fn ws(name: &str) -> Workspace {
        let mut w = Workspace::new(WorkspaceId::new(), name);
        w.owner = Some(local_user_id());
        w
    }

    fn tree_with(workspaces: Vec<Workspace>) -> WorkspaceTree {
        let active = workspaces.first().map(|w| w.id).unwrap_or_default();
        WorkspaceTree {
            desktop: crate::shell::state::DesktopTree {
                workspaces,
                active,
                ..Default::default()
            },
            ..WorkspaceTree::default()
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn room_access_distinguishes_members_shares_and_expired_grants() {
        fn a_room_only_you_own_reads_as_private() {
            let tree = tree_with(vec![ws("Home")]);
            let cards = room_cards(&tree);
            assert_eq!(cards.len(), 1);
            assert!(cards[0].is_private());
            assert_eq!(room_access_label(&cards[0]), "Private to you");
            assert_eq!(room_shares_label(&cards[0]), None);
        }

        fn a_member_makes_the_room_read_as_shared_with_them() {
            let mut w = ws("Design");
            let room = w.id.raw().to_string();
            w.capabilities.push(CapOverride::grant(
                Subject::person("ada"),
                Scope::Room(room),
                Verb::Write,
            ));
            w.collaborators.push(Collaborator::new("ada", "Ada"));
            let tree = tree_with(vec![w]);
            let card = &room_cards(&tree)[0];
            assert!(!card.is_private());
            assert_eq!(room_access_label(card), "Shared with Ada");
            // Membership is not a resource share — the two lists stay disjoint.
            assert_eq!(room_shares_label(card), None);
        }

        fn a_file_grant_is_a_share_and_not_a_membership() {
            let mut w = ws("Design");
            w.capabilities.push(CapOverride::grant(
                Subject::person("ada"),
                Scope::FsPath("/var/workspace/brief".to_string()),
                Verb::Read,
            ));
            let tree = tree_with(vec![w]);
            let card = &room_cards(&tree)[0];
            // Ada holds a file, not the room: she is not a member…
            assert!(card.is_private(), "a file grant must not admit a member");
            assert_eq!(room_access_label(card), "Private to you");
            // …and the file shows up as exactly one share, named the way the
            // permissions pane names it.
            assert_eq!(card.shares.len(), 1);
            assert_eq!(card.shares[0].with, vec!["ada".to_string()]);
            assert_eq!(room_shares_label(card).as_deref(), Some("1 item shared"));
        }

        fn two_grants_on_one_path_are_one_share_with_two_holders() {
            let mut w = ws("Design");
            for who in ["ada", "bob"] {
                w.capabilities.push(CapOverride::grant(
                    Subject::person(who),
                    Scope::FsPath("/var/workspace/brief".to_string()),
                    Verb::Read,
                ));
            }
            let tree = tree_with(vec![w]);
            let card = &room_cards(&tree)[0];
            assert_eq!(card.shares.len(), 1, "one path is one share");
            assert_eq!(card.shares[0].with.len(), 2);
            assert_eq!(room_shares_label(card).as_deref(), Some("1 item shared"));
        }

        fn write_grants_are_marked_writable() {
            let mut w = ws("Design");
            w.capabilities.push(CapOverride::grant(
                Subject::person("ada"),
                Scope::FsPath("/var/workspace/brief".to_string()),
                Verb::Write,
            ));
            let tree = tree_with(vec![w]);
            let card = &room_cards(&tree)[0];
            assert!(card.shares[0].writable);
        }

        fn expired_grants_neither_admit_members_nor_count_as_shares() {
            use crate::shell::state::Expiry;
            let mut w = ws("Design");
            let room = w.id.raw().to_string();
            w.capabilities.push(
                CapOverride::grant(Subject::person("ada"), Scope::Room(room), Verb::Write)
                    .expiring(Expiry::AtUnix(1)),
            );
            w.capabilities.push(
                CapOverride::grant(
                    Subject::person("ada"),
                    Scope::FsPath("/var/workspace/brief".to_string()),
                    Verb::Read,
                )
                .expiring(Expiry::AtUnix(1)),
            );
            let tree = tree_with(vec![w]);
            let card = &room_cards(&tree)[0];
            assert!(card.is_private(), "an expired membership admits nobody");
            assert!(card.shares.is_empty(), "an expired share is not shared");
        }

        fn access_label_counts_once_naming_stops_helping() {
            let mut w = ws("Design");
            let room = w.id.raw().to_string();
            for who in ["ada", "bob", "cleo", "dara"] {
                w.capabilities.push(CapOverride::grant(
                    Subject::person(who),
                    Scope::Room(room.clone()),
                    Verb::Write,
                ));
            }
            let tree = tree_with(vec![w]);
            let card = &room_cards(&tree)[0];
            assert_eq!(room_access_label(card), "Shared with ada and 3 others");
        }

        a_room_only_you_own_reads_as_private();
        a_member_makes_the_room_read_as_shared_with_them();
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
    fn room_presence_is_who_is_here_now_and_is_attached_to_tabs() {
        fn presence_is_who_is_here_now_not_who_is_a_member() {
            let mut w = ws("Design");
            let room = w.id.raw().to_string();
            for who in ["ada", "bob"] {
                w.capabilities.push(CapOverride::grant(
                    Subject::person(who),
                    Scope::Room(room.clone()),
                    Verb::Write,
                ));
            }
            // Only Ada actually showed up.
            w.collaborators.push(Collaborator::new("ada", "Ada"));
            let tree = tree_with(vec![w]);
            let card = &room_cards(&tree)[0];
            assert_eq!(card.people.len(), 3, "you + two members");
            assert_eq!(card.present_count(), 2, "you and Ada, not Bob");
            assert_eq!(
                room_presence_label(card).as_deref(),
                Some("You and Ada are here")
            );
        }

        fn a_room_you_are_not_in_does_not_report_you_as_here() {
            let here = ws("Home");
            let elsewhere = ws("Design");
            let tree = tree_with(vec![here, elsewhere]);
            let cards = room_cards(&tree);
            assert_eq!(
                room_presence_label(&cards[0]).as_deref(),
                Some("You are here")
            );
            assert_eq!(
                room_presence_label(&cards[1]),
                None,
                "you cannot be in two rooms at once"
            );
        }

        fn private_overview_stays_quiet_but_shared_overview_names_presence() {
            let private_tree = tree_with(vec![ws("Home")]);
            assert_eq!(overview_people_label(&room_cards(&private_tree)[0]), None);

            let mut shared = ws("Design");
            shared.capabilities.push(CapOverride::grant(
                Subject::person("ada"),
                Scope::Room(shared.id.raw().to_string()),
                Verb::Write,
            ));
            shared.collaborators.push(Collaborator::new("ada", "Ada"));
            let shared_tree = tree_with(vec![shared]);
            assert_eq!(
                overview_people_label(&room_cards(&shared_tree)[0]).as_deref(),
                Some("Ada is here")
            );
        }

        fn spatial_projection_preserves_strip_group_and_tab_nesting() {
            let mut w = ws("Design");
            let strip_id = StripId::new();
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
                        },
                        Tab {
                            id: second_tab,
                            title: "References".to_string(),
                            tile: TileContent::Stub {
                                kind: AppKind::Browser,
                                label: "References".to_string(),
                            },
                        },
                    ],
                    active_tab: second_tab,
                    width: WindowWidth::Wide,
                    maximized: false,
                }],
            }];
            let mut tree = tree_with(vec![w]);
            let ws_id = tree.desktop.active;
            tree.desktop.active_strip.insert(ws_id, strip_id);
            tree.desktop.set_active_group(strip_id, group_id);

            let card = &room_cards(&tree)[0];
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
                "the room row names each open app once, in tree order"
            );
        }

        fn collaborator_focus_is_attached_to_the_tab_not_the_workspace_header() {
            let mut w = ws("Design");
            let strip_id = StripId::new();
            let tab_id = 71;
            w.capabilities.push(CapOverride::grant(
                Subject::person("ada"),
                Scope::Room(w.id.raw().to_string()),
                Verb::Write,
            ));
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
                    }],
                    active_tab: tab_id,
                    width: WindowWidth::Normal,
                    maximized: false,
                }],
            }];
            let tree = tree_with(vec![w]);

            let tab = &room_cards(&tree)[0].strips[0].groups[0].tabs[0];
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
        a_room_you_are_not_in_does_not_report_you_as_here();
        private_overview_stays_quiet_but_shared_overview_names_presence();
        spatial_projection_preserves_strip_group_and_tab_nesting();
        collaborator_focus_is_attached_to_the_tab_not_the_workspace_header();
    }
}
