//! The strips a persistent room's layout has: the named places its operator
//! lists, so the first person in finds them in the top bar.
//!
//! The layout is the room's own document
//! ([`tilo_desktop_model::room_layout`]): one workspace whose strips, window
//! groups and windows live in the room's `contents`, each entry written by
//! the device whose namespace it carries. The operator's strips are minted in
//! a namespace of the room's own, so they belong to nobody in particular and
//! every member sees them. A listed strip is added where its name is
//! missing; everything people made, strips and windows alike, is kept.

use tilo_desktop_model::{DesktopTree, IdMinter, RoomId, Strip, StripId, WorkspaceId, room_layout};

/// The room record members' pages keep the room's layout in.
pub(crate) const DESKTOP: (&str, &str) = ("so.tilo.browser-desktop", "desktop");
/// The longest strip name, the same as one a person names in the top bar.
const STRIP_NAME_MAX: usize = 48;

pub(crate) fn strip_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > STRIP_NAME_MAX {
        return Err(format!(
            "a strip name is 1-{STRIP_NAME_MAX} characters, not {name:?}"
        ));
    }
    Ok(name.to_owned())
}

/// Give the room's layout a strip named each of `names`, in order, in its
/// workspace where none has that name yet. A room with no layout starts one:
/// a workspace named `shown` holding those strips. Returns whether it
/// changed the layout.
pub(crate) fn settle_strips(
    doc: &mut automerge::AutoCommit,
    room: &str,
    shown: &str,
    names: &[String],
) -> Result<bool, String> {
    if names.is_empty() {
        return Ok(false);
    }
    let Some(room_id) = RoomId::parse(room) else {
        return Err(format!("not a room: {room}"));
    };
    let mut layout = room_layout::read(doc, None).unwrap_or_else(|| {
        let workspace =
            tilo_desktop_model::Workspace::in_room(WorkspaceId::of_room(room_id), room_id, shown);
        tilo_desktop_model::ownership::SharedDesktop {
            tree: DesktopTree {
                active: workspace.id,
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            ..Default::default()
        }
    });
    if layout.tree.workspaces.is_empty() {
        return Ok(false);
    }
    let mut minter = Minter::new(room, &layout.tree);
    let Some(workspace) = layout.tree.workspaces.first_mut() else {
        return Ok(false);
    };
    let mut changed = false;
    for name in names {
        if workspace.strips.iter().any(|strip| &strip.name == name) {
            continue;
        }
        workspace
            .strips
            .push(Strip::new(StripId(minter.mint()), name.clone()));
        changed = true;
    }
    if !changed {
        return Ok(false);
    }
    let salt = Minter::salt(room);
    room_layout::write(doc, &layout, salt, None).map_err(|error| error.to_string())
}

/// Ids for the strips the room adds: under a namespace of the room's own,
/// never one the layout already holds.
struct Minter {
    ids: IdMinter,
    taken: std::collections::HashSet<u64>,
}

impl Minter {
    fn salt(room: &str) -> u16 {
        let hash = blake3::hash(format!("room strips {room}").as_bytes());
        let [a, b, ..] = *hash.as_bytes();
        // Salt 0 is no namespace at all, and the reserved one is nobody's.
        tilo_desktop_model::ids::device_salt(u16::from_le_bytes([a, b]))
    }

    fn new(room: &str, tree: &DesktopTree) -> Self {
        let mut taken = std::collections::HashSet::new();
        for workspace in &tree.workspaces {
            taken.insert(workspace.id.0);
            for strip in &workspace.strips {
                taken.insert(strip.id.0);
                for group in &strip.window_groups {
                    taken.insert(group.id);
                    taken.extend(group.tabs.iter().map(|tab| tab.id));
                }
            }
        }
        Self {
            ids: IdMinter::new(Self::salt(room)),
            taken,
        }
    }

    fn mint(&mut self) -> u64 {
        loop {
            let id = self.ids.mint();
            if self.taken.insert(id) {
                return id;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::ownership::SharedDesktop;

    const ROOM: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    fn desktop(doc: &automerge::AutoCommit) -> SharedDesktop {
        room_layout::read(doc, None).unwrap()
    }

    fn strips(doc: &automerge::AutoCommit) -> Vec<String> {
        desktop(doc).tree.workspaces[0]
            .strips
            .iter()
            .map(|strip| strip.name.clone())
            .collect()
    }

    /// A room without a layout starts one: a workspace with the room's name
    /// and the listed strips.
    #[test]
    fn a_room_without_a_layout_starts_with_the_listed_strips() {
        let mut doc = automerge::AutoCommit::new();
        assert!(settle_strips(&mut doc, ROOM, "Hangout", &names(&["Main", "Games"])).unwrap());
        assert_eq!(strips(&doc), ["Main", "Games"]);
        let tree: DesktopTree = desktop(&doc).tree;
        assert_eq!(tree.workspaces[0].name, "Hangout");
        assert_eq!(tree.active, tree.workspaces[0].id);
        assert_eq!(
            tree.workspaces[0].id,
            WorkspaceId::of_room(RoomId::parse(ROOM).unwrap()),
            "the room's own workspace"
        );
    }

    /// Settling again changes nothing; a newly listed strip is added after
    /// what is there, and the strips people made and who made them are kept.
    #[test]
    fn settling_adds_only_what_is_missing() {
        let mut doc = automerge::AutoCommit::new();
        settle_strips(&mut doc, ROOM, "Hangout", &names(&["Main"])).unwrap();
        let alice = 0x0A11;
        let mut value = desktop(&doc);
        value.tree.workspaces[0].strips.push(Strip::new(
            StripId(tilo_desktop_model::compose_id(alice, 1)),
            "Someone's",
        ));
        value
            .authors
            .insert(tilo_desktop_model::compose_id(alice, 1), "alice".into());
        room_layout::write(&mut doc, &value, alice, Some("alice")).unwrap();

        assert!(!settle_strips(&mut doc, ROOM, "Hangout", &names(&["Main"])).unwrap());
        assert!(settle_strips(&mut doc, ROOM, "Hangout", &names(&["Main", "Quiet"])).unwrap());
        assert_eq!(strips(&doc), ["Main", "Someone's", "Quiet"]);
        let value = desktop(&doc);
        assert_eq!(
            value
                .authors
                .get(&tilo_desktop_model::compose_id(alice, 1))
                .map(String::as_str),
            Some("alice")
        );
        let ids: std::collections::HashSet<_> = value.tree.workspaces[0]
            .strips
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids.len(), 3, "a new strip never reuses an id");
    }

    /// No listed strips leaves the layout alone, even an absent one.
    #[test]
    fn no_strips_writes_nothing() {
        let mut doc = automerge::AutoCommit::new();
        assert!(!settle_strips(&mut doc, ROOM, "Hangout", &[]).unwrap());
        assert!(room_layout::read(&doc, None).is_none());
    }

    #[test]
    fn strip_names_are_trimmed_and_bounded() {
        assert_eq!(strip_name("  Games night ").unwrap(), "Games night");
        assert!(strip_name("   ").is_err());
        assert!(strip_name(&"x".repeat(49)).is_err());
        assert!(strip_name(&"é".repeat(48)).is_ok());
    }
}
