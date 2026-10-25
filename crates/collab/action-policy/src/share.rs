//! Sharing one resource with a room: the grants a share is.
//!
//! A person who shares something they work on (a note shown live in a chat
//! channel) lets the room's members see it, or also change it. That is not
//! a promise kept by the sharing app: a share is grants on the shared
//! resource's own node, decided by [`crate::decide`] at every door and by
//! every receiver like any other grant, and revoked when the share ends.
//!
//! - **One resource.** A share names one record
//!   (`room › app › kind › resource`) or one item of it
//!   (`… › resource › item`). An app's own record of the room travels under
//!   an empty route that a node cannot name, so an item of it is named under
//!   `resource:*`: item ids are unique, so the node stays one item. A bare
//!   `resource:*` would be every record of the kind and is refused.
//! - **Everyone in the room below the owner.** A grant per role preset,
//!   Viewer to Moderator: Read for a view share, Read and Undoable for an
//!   edit share. A grant covers only the kinds it names, so a view share
//!   strips no one: an Editor's Undoable on the shared item is still decided
//!   by their role's grant on the room.
//! - **Never more than the sharer holds.** Each grant is bounded by the
//!   sharer's own holdings on its node ([`check_held`]), so a Viewer cannot
//!   hand out edit.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{
    ActionKind, Answer, Authority, Context, Grant, KindSet, Level, Role, RoomPolicy, Segment,
    Subject, TreePath,
};

/// What a share lets the room do with the resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareAccess {
    /// See it.
    View,
    /// See it and make undoable changes to it.
    Edit,
}

impl ShareAccess {
    /// The kinds the share's grants cover.
    #[must_use]
    pub const fn kinds(self) -> KindSet {
        match self {
            Self::View => KindSet::of(ActionKind::Read),
            Self::Edit => KindSet::of(ActionKind::Read).with(ActionKind::Undoable),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Edit => "edit",
        }
    }
}

impl FromStr for ShareAccess {
    type Err = ShareRefusal;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "view" => Ok(Self::View),
            "edit" => Ok(Self::Edit),
            _ => Err(ShareRefusal::Access(text.to_owned())),
        }
    }
}

/// Why a share cannot be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareRefusal {
    /// The share's id is empty, too long or has characters ids may not.
    Id,
    /// The node is not one record or one item of a record of this room.
    Node(String),
    /// The access is neither `view` nor `edit`.
    Access(String),
    /// The sharer does not hold what the share would give.
    NotHeld(crate::Refusal),
}

impl std::fmt::Display for ShareRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Id => f.write_str("a share's id is 1 to 128 letters, digits or . _ : -"),
            Self::Node(why) => write!(f, "only one record or one item of it can be shared: {why}"),
            Self::Access(access) => write!(f, "a share gives `view` or `edit`, not `{access}`"),
            Self::NotHeld(refusal) => write!(f, "you can't share more than you hold: {refusal}"),
        }
    }
}

impl std::error::Error for ShareRefusal {}

/// The longest share id.
const MAX_SHARE_ID: usize = 128;

/// The roles a share reaches: every preset below the owner, who holds
/// everything already.
const SHARED_WITH: [Role; 4] = [Role::Viewer, Role::Commenter, Role::Editor, Role::Moderator];

/// The start of the id of every grant of the share `share`.
#[must_use]
pub fn share_id_prefix(share: &str) -> String {
    format!("share:{share}:")
}

/// Whether `grant` is one of the share `share`'s.
#[must_use]
pub fn is_of_share(grant: &Grant, share: &str) -> bool {
    grant.id.as_str().starts_with(&share_id_prefix(share))
}

/// The grants `sharer` gives the room `room` by sharing `node` with
/// `access`, under the share id `share` (unique per share, such as the id of
/// the message that shares it).
///
/// # Errors
/// [`ShareRefusal::Id`] for a malformed id and [`ShareRefusal::Node`] for a
/// node that is not one record or one item of it in `room`.
pub fn share_grants(
    room: &str,
    sharer: &str,
    share: &str,
    node: &TreePath,
    access: ShareAccess,
) -> Result<Vec<Grant>, ShareRefusal> {
    if share.is_empty()
        || share.len() > MAX_SHARE_ID
        || !share
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '-'))
    {
        return Err(ShareRefusal::Id);
    }
    check_node(room, node)?;
    Ok(SHARED_WITH
        .into_iter()
        .map(|role| {
            Grant::new(
                format!(
                    "{}{}",
                    share_id_prefix(share),
                    role.name().to_ascii_lowercase()
                ),
                Subject::Role(role),
                node.clone(),
                access.kinds(),
                Answer::Allow,
                Subject::Person(sharer.to_owned()),
            )
        })
        .collect())
}

/// Whether `node` names one record of `room`, or one item of a record.
fn check_node(room: &str, node: &TreePath) -> Result<(), ShareRefusal> {
    let named = |segment: Option<&Segment>, level: Level| matches!(segment, Some(Segment::Name(at, id)) if *at == level && !id.is_empty());
    let segments = node.segments();
    if node.room_id() != Some(room) {
        return Err(ShareRefusal::Node("it is not in this room".into()));
    }
    if !(named(segments.first(), Level::Room)
        && named(segments.get(1), Level::App)
        && named(segments.get(2), Level::Kind))
    {
        return Err(ShareRefusal::Node("it names no app and kind".into()));
    }
    let item = match segments.len() {
        4 => false,
        5 if named(segments.get(4), Level::Item) => true,
        _ => return Err(ShareRefusal::Node("it is not a record or an item".into())),
    };
    match segments.get(3) {
        Some(segment) if named(Some(segment), Level::Resource) => Ok(()),
        // An item of the app's own record, which no name names.
        Some(Segment::Any(Level::Resource)) if item => Ok(()),
        _ => Err(ShareRefusal::Node(
            "it names no one record (every record of a kind can't be shared)".into(),
        )),
    }
}

/// Whether `sharer`, deciding as `actor`, holds everything each of
/// `grants` gives on its node.
///
/// # Errors
/// [`ShareRefusal::NotHeld`] for the first grant above the sharer's own.
pub fn check_held(
    actor: &Authority<'_>,
    grants: &[Grant],
    cx: &Context<'_>,
) -> Result<(), ShareRefusal> {
    grants
        .iter()
        .try_for_each(|grant| RoomPolicy::check_within(actor, grant, cx))
        .map_err(ShareRefusal::NotHeld)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: &str = "r1";

    fn item(id: &str) -> TreePath {
        TreePath::room(ROOM)
            .app("so.tilo.notes")
            .kind("notes.notebook")
            .any(Level::Resource)
            .item(id)
    }

    fn edit_at(node: &TreePath) -> TreePath {
        node.clone().field("body").action("field-edit")
    }

    /// A room owned by `ada` with `bo` as a Viewer and `cy` as an Editor.
    fn room() -> RoomPolicy {
        let mut policy = RoomPolicy::new(ROOM, "ada");
        policy.members.insert("bo".into(), Role::Viewer);
        policy.members.insert("cy".into(), Role::Editor);
        policy
    }

    fn may(policy: &RoomPolicy, person: &str, node: &TreePath, kind: ActionKind) -> bool {
        Authority::person(policy.party(person))
            .decide(node, kind, &Context::at(0))
            .answer
            == Answer::Allow
    }

    fn shared(access: ShareAccess) -> RoomPolicy {
        let mut policy = room();
        let grants = share_grants(ROOM, "ada", "m1", &item("p1"), access).unwrap();
        check_held(
            &Authority::person(policy.party("ada")),
            &grants,
            &Context::at(0),
        )
        .unwrap();
        policy.grants.extend(grants);
        policy
    }

    #[test]
    fn an_edit_share_lets_a_viewer_change_that_item_only() {
        let before = room();
        assert!(!may(
            &before,
            "bo",
            &edit_at(&item("p1")),
            ActionKind::Undoable
        ));
        let edit = shared(ShareAccess::Edit);
        assert!(may(
            &edit,
            "bo",
            &edit_at(&item("p1")),
            ActionKind::Undoable
        ));
        assert!(
            !may(&edit, "bo", &edit_at(&item("p2")), ActionKind::Undoable),
            "a sibling item stays as the role has it"
        );
        let view = shared(ShareAccess::View);
        assert!(!may(
            &view,
            "bo",
            &edit_at(&item("p1")),
            ActionKind::Undoable
        ));
        assert!(may(&view, "bo", &edit_at(&item("p1")), ActionKind::Read));
    }

    #[test]
    fn a_view_share_strips_no_one() {
        let view = shared(ShareAccess::View);
        assert!(
            may(&view, "cy", &edit_at(&item("p1")), ActionKind::Undoable),
            "an Editor still edits the shared item through their role"
        );
    }

    #[test]
    fn a_share_gives_no_more_than_its_sharer_holds() {
        let policy = room();
        let grants = share_grants(ROOM, "bo", "m1", &item("p1"), ShareAccess::Edit).unwrap();
        assert!(matches!(
            check_held(
                &Authority::person(policy.party("bo")),
                &grants,
                &Context::at(0)
            ),
            Err(ShareRefusal::NotHeld(_))
        ));
        let grants = share_grants(ROOM, "bo", "m1", &item("p1"), ShareAccess::View).unwrap();
        assert!(
            check_held(
                &Authority::person(policy.party("bo")),
                &grants,
                &Context::at(0)
            )
            .is_ok()
        );
    }

    #[test]
    fn a_share_is_one_record_or_one_item_of_this_room() {
        let record = TreePath::room(ROOM)
            .app("so.tilo.notes")
            .kind("notes.notebook")
            .resource("plan");
        let ok = |node: &TreePath| share_grants(ROOM, "ada", "m1", node, ShareAccess::View);
        assert_eq!(ok(&record).unwrap().len(), 4);
        assert!(ok(&record.clone().item("p1")).is_ok());
        assert!(ok(&item("p1")).is_ok());
        let every = TreePath::room(ROOM)
            .app("so.tilo.notes")
            .kind("notes.notebook")
            .any(Level::Resource);
        for refused in [
            every,
            TreePath::room(ROOM).app("so.tilo.notes"),
            TreePath::room("r2")
                .app("so.tilo.notes")
                .kind("notes.notebook")
                .resource("plan"),
            record.clone().item("p1").field("body"),
            TreePath::room(ROOM).rest(),
        ] {
            assert!(
                matches!(ok(&refused), Err(ShareRefusal::Node(_))),
                "{refused}"
            );
        }
        assert_eq!(
            share_grants(ROOM, "ada", "", &record, ShareAccess::View),
            Err(ShareRefusal::Id)
        );
        assert_eq!(
            share_grants(ROOM, "ada", "a/b", &record, ShareAccess::View),
            Err(ShareRefusal::Id)
        );
        let grants = ok(&record).unwrap();
        assert!(grants.iter().all(|grant| is_of_share(grant, "m1")
            && grant.granted_by == Subject::Person("ada".into())
            && grant.answer == Answer::Allow));
        assert!(
            !grants
                .iter()
                .any(|grant| grant.subject == Subject::Role(Role::Owner))
        );
        assert_eq!("edit".parse::<ShareAccess>(), Ok(ShareAccess::Edit));
        assert!("own".parse::<ShareAccess>().is_err());
    }
}
