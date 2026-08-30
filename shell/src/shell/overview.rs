//! What the workspace overview is made of.
//!
//! A workspace in qOS is a **room** — a host, people, permissions,
//! shared files — so the overview answers questions about rooms (who
//! else is here, what is shared), not window/tab counts. Derived from
//! the authoritative [`WorkspaceTree`] as plain data, so the draw pass
//! has no vocabulary of its own to drift with.

use crate::shell::state::{
    CapRole, Collaborator, Effect, Principal, Subject, UserId, Workspace, WorkspaceId,
    WorkspaceTree, local_user_id, now_unix,
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
            .workspaces
            .iter()
            .map(|w| CardCache {
                id: w.id,
                name: w.name.clone(),
                strip_count: w.strips.len(),
            })
            .collect();
        self.active = Some(tree.active);
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
            self.active = Some(tree.active);
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomCard {
    pub id: WorkspaceId,
    pub name: String,
    pub is_current: bool,
    /// Owner first.
    pub people: Vec<RoomPerson>,
    /// Most-shared first.
    pub shares: Vec<RoomShare>,
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
        .chain(tree.collaborators.iter())
        .find(|c| &c.id == id)
}

pub fn room_people(ws: &Workspace, tree: &WorkspaceTree) -> Vec<RoomPerson> {
    let me = local_user_id();
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
                || (id == me && ws.id == tree.active);
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
    let now = Some(now_unix());
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
    tree.workspaces
        .iter()
        .map(|ws| RoomCard {
            id: ws.id,
            name: ws.name.clone(),
            is_current: ws.id == tree.active,
            people: room_people(ws, tree),
            shares: room_shares(ws, tree),
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
                "No members".to_string()
            } else {
                "Private to you".to_string()
            }
        }
        1 => format!(
            "Shared with {}",
            others.first().map_or("", |p| p.name.as_str())
        ),
        2 => format!(
            "Shared with {} and {}",
            others.first().map_or("", |p| p.name.as_str()),
            others.get(1).map_or("", |p| p.name.as_str())
        ),
        n => format!(
            "Shared with {} and {} others",
            others.first().map_or("", |p| p.name.as_str()),
            n.saturating_sub(1)
        ),
    }
}

/// `None` when it shares nothing — the caller draws nothing rather than a zero.
pub fn room_shares_label(card: &RoomCard) -> Option<String> {
    match card.shares.len() {
        0 => None,
        1 => Some("1 item shared".to_string()),
        n => Some(format!("{n} items shared")),
    }
}

/// `None` when nobody is here, including you — every room you aren't in.
pub fn room_presence_label(card: &RoomCard) -> Option<String> {
    let live: Vec<&RoomPerson> = card.people.iter().filter(|p| p.present).collect();
    let others: Vec<&&RoomPerson> = live.iter().filter(|p| !p.is_you).collect();
    match (live.iter().any(|p| p.is_you), others.len()) {
        (false, 0) => None,
        (true, 0) => Some("You are here".to_string()),
        (true, 1) => Some(format!(
            "You and {} are here",
            others.first().map_or("", |p| p.name.as_str())
        )),
        (true, n) => Some(format!("You and {n} others are here")),
        (false, 1) => Some(format!(
            "{} is here",
            others.first().map_or("", |p| p.name.as_str())
        )),
        (false, n) => Some(format!("{n} people are here")),
    }
}

#[cfg(test)]
mod room_tests {
    use super::*;
    use crate::shell::state::{CapOverride, Scope, Verb};

    fn ws(name: &str) -> Workspace {
        let mut w = Workspace::new(WorkspaceId::new(), name);
        w.owner = Some(local_user_id());
        w
    }

    fn tree_with(workspaces: Vec<Workspace>) -> WorkspaceTree {
        let active = workspaces.first().map(|w| w.id).unwrap_or_default();
        WorkspaceTree {
            active,
            workspaces,
            ..WorkspaceTree::default()
        }
    }

    #[test]
    fn a_room_only_you_own_reads_as_private() {
        let tree = tree_with(vec![ws("Home")]);
        let cards = room_cards(&tree);
        assert_eq!(cards.len(), 1);
        assert!(cards[0].is_private());
        assert_eq!(room_access_label(&cards[0]), "Private to you");
        assert_eq!(room_shares_label(&cards[0]), None);
    }

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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
}
