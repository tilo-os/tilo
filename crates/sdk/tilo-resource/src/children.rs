//! Resources that contain resources: a container lists its children, each a
//! record of its own.
//!
//! A child that can need its own permissions, sharing or history (a chat's
//! channel, a category) is its own record, with a minted id that never
//! changes, living in its parent's room. The parent keeps only what places
//! it: a [`ChildRef`] per child in a [`Children`] field, in order, naming its
//! kind, the sibling it is grouped under (a channel's category) and whether
//! it is in the room's trash. Moving a child moves its reference; the child
//! keeps its identity, so every reference to it keeps working.
//!
//! Every container has the same lifecycle (the framework publishes it, the
//! app writes no verbs for it): `create` a child of a kind at a place with
//! its first fields, `delete` it to the trash (restorable, its record and id
//! kept), `restore` it, and `move` it to another place or group.

use crate::{Atom, Collection, Id, Model};

/// The id of the generated action that makes a child.
pub const CREATE: &str = "create";
/// The id of the generated action that puts a child in the trash.
pub const DELETE: &str = "delete";
/// The id of the generated action that takes a child out of the trash.
pub const RESTORE: &str = "restore";
/// The id of the generated action that moves a child.
pub const MOVE: &str = "move";

/// When a child went to the room's trash: in milliseconds since the epoch.
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trashed {
    /// When.
    pub at: i64,
}

/// One child a container lists. See the [module docs](self).
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct ChildRef {
    /// Its resource kind: a container may hold several, and a child is
    /// always read as the kind it was made as.
    #[model(readonly)]
    pub kind: Atom,
    /// The sibling it is listed under (a channel's category), if any.
    pub group: Option<Id>,
    /// In the room's trash since then: listed nowhere, kept whole.
    pub trashed: Option<Trashed>,
}

/// The children a container lists, in order. See the [module docs](self).
pub type Children = Collection<ChildRef>;

/// Reading a container: what it lists outside the trash, by kind and group.
pub trait ChildList {
    /// Every child outside the trash, in order.
    fn listed(&self) -> Vec<(&Id, &ChildRef)>;
    /// The children of `kind` outside the trash, in order.
    fn listed_of(&self, kind: &str) -> Vec<&Id>;
    /// The children listed under `group` (none: at the top), outside the
    /// trash, in order.
    fn under(&self, group: Option<&Id>) -> Vec<&Id>;
    /// The children in the trash, in order.
    fn in_trash(&self) -> Vec<&Id>;
    /// Whether `id` is listed outside the trash.
    fn lists(&self, id: &Id) -> bool;
}

impl ChildList for Children {
    fn listed(&self) -> Vec<(&Id, &ChildRef)> {
        self.iter()
            .filter(|(_, child)| child.trashed.is_none())
            .collect()
    }

    fn listed_of(&self, kind: &str) -> Vec<&Id> {
        self.iter()
            .filter(|(_, child)| child.trashed.is_none() && child.kind.0 == kind)
            .map(|(id, _)| id)
            .collect()
    }

    fn under(&self, group: Option<&Id>) -> Vec<&Id> {
        self.iter()
            .filter(|(_, child)| child.trashed.is_none() && child.group.as_ref() == group)
            .map(|(id, _)| id)
            .collect()
    }

    fn in_trash(&self) -> Vec<&Id> {
        self.iter()
            .filter(|(_, child)| child.trashed.is_some())
            .map(|(id, _)| id)
            .collect()
    }

    fn lists(&self, id: &Id) -> bool {
        self.get(id).is_some_and(|child| child.trashed.is_none())
    }
}

/// Where a moved or new child goes in the container's order: before the
/// sibling `before`, else last (and so last among the children of whichever
/// group it is listed under).
#[must_use]
pub fn place(children: &Children, before: Option<&Id>) -> usize {
    before
        .and_then(|before| children.position(before))
        .unwrap_or_else(|| children.len())
}

/// Why a child cannot be grouped under `group`: it names no listed child,
/// or the child itself (nothing is listed under itself).
///
/// # Errors
/// The refusal, as the lifecycle actions say it.
pub fn check_group(children: &Children, child: &Id, group: Option<&Id>) -> Result<(), String> {
    match group {
        Some(group) if group == child => Err("nothing is listed under itself".into()),
        Some(group) if !children.lists(group) => Err("there is no such group".into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(kind: &str, group: Option<&str>) -> ChildRef {
        ChildRef {
            kind: Atom(kind.into()),
            group: group.map(Id::from_raw),
            trashed: None,
        }
    }

    #[test]
    fn a_container_lists_its_children_by_group_and_keeps_the_trashed_aside() {
        let mut children = Children::new();
        children.push(Id::from_raw("design"), child("chat.category", None));
        children.push(Id::from_raw("general"), child("chat.channel", None));
        children.push(Id::from_raw("ui"), child("chat.channel", Some("design")));
        children.push(Id::from_raw("ux"), child("chat.channel", Some("design")));
        assert_eq!(
            children.listed_of("chat.channel"),
            [
                &Id::from_raw("general"),
                &Id::from_raw("ui"),
                &Id::from_raw("ux")
            ]
        );
        assert_eq!(
            children.under(Some(&Id::from_raw("design"))),
            [&Id::from_raw("ui"), &Id::from_raw("ux")]
        );
        children.get_mut(&Id::from_raw("ui")).unwrap().trashed = Some(Trashed { at: 5 });
        assert_eq!(
            children.under(Some(&Id::from_raw("design"))),
            [&Id::from_raw("ux")]
        );
        assert_eq!(children.in_trash(), [&Id::from_raw("ui")]);
        assert!(!children.lists(&Id::from_raw("ui")));
        assert!(children.contains(&Id::from_raw("ui")), "kept whole");
    }

    #[test]
    fn a_new_child_goes_last_or_before_the_sibling_named() {
        let mut children = Children::new();
        children.push(Id::from_raw("design"), child("chat.category", None));
        children.push(Id::from_raw("ui"), child("chat.channel", Some("design")));
        children.push(Id::from_raw("general"), child("chat.channel", None));
        let design = Id::from_raw("design");
        assert_eq!(place(&children, None), 3);
        assert_eq!(place(&children, Some(&Id::from_raw("ui"))), 1);
        assert_eq!(place(&children, Some(&Id::from_raw("absent"))), 3);
        assert_eq!(place(&Children::new(), None), 0);
        assert!(check_group(&children, &Id::from_raw("ui"), Some(&design)).is_ok());
        assert!(check_group(&children, &design, Some(&design)).is_err());
        assert!(check_group(&children, &design, Some(&Id::from_raw("absent"))).is_err());
    }
}
