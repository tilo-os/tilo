//! The strips a persistent workspace's shared desktop has: the named places its
//! operator lists, so the first person in finds them in the top bar. The
//! desktop is the record members' pages keep their windows in, one entry
//! per workspace, strip, group and window
//! ([`tilo_desktop_model::shared_record`]). A listed strip is added where
//! its name is missing; everything people made, strips and windows alike,
//! is kept.

use tilo_desktop_model::{
    DesktopTree, IdMinter, Strip, StripId, Workspace, WorkspaceId, WorkspaceSlot, shared_record,
};

/// The workspace record members' pages keep the shared desktop in.
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

/// Give the workspace's shared desktop a strip named each of `names`, in order,
/// in its first workspace where none has that name yet. A workspace with no
/// desktop starts one: a workspace named `shown` holding those strips.
/// Returns whether it changed the desktop.
pub(crate) fn settle_strips(
    doc: &mut automerge::AutoCommit,
    workspace: &str,
    shown: &str,
    names: &[String],
) -> Result<bool, String> {
    if names.is_empty() {
        return Ok(false);
    }
    let mut desktop = shared_record::read(doc, None).unwrap_or_default();
    let tree = &mut desktop.tree;
    let mut minter = Minter::new(workspace, tree);
    if tree.workspaces.is_empty() {
        // The desktop's workspace is the workspace itself, by its id, as every member's desk is.
        let identity = WorkspaceId::parse(workspace)
            .ok_or_else(|| format!("{workspace:?} is not a workspace id"))?;
        let workspace = Workspace::in_workspace(WorkspaceSlot(minter.mint()), identity, shown);
        tree.active = workspace.slot;
        tree.workspaces.push(workspace);
    }
    let Some(workspace) = tree.workspaces.first_mut() else {
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
    shared_record::write(doc, &desktop).map_err(|error| error.to_string())
}

/// Ids for the strips the workspace adds: under a namespace of the workspace's own,
/// never one the desktop already holds.
struct Minter {
    ids: IdMinter,
    taken: std::collections::HashSet<u64>,
}

impl Minter {
    fn new(workspace: &str, tree: &DesktopTree) -> Self {
        // The salt's derivation keeps the bytes it was first derived with.
        let hash = blake3::hash(format!("room strips {workspace}").as_bytes());
        let [a, b, ..] = *hash.as_bytes();
        // Salt 0 is no namespace at all, and the reserved one is nobody's.
        let salt = tilo_desktop_model::ids::device_salt(u16::from_le_bytes([a, b]));
        let mut taken = std::collections::HashSet::new();
        for workspace in &tree.workspaces {
            taken.insert(workspace.slot.0);
            for strip in &workspace.strips {
                taken.insert(strip.id.0);
                for group in &strip.window_groups {
                    taken.insert(group.id);
                    taken.extend(group.tabs.iter().map(|tab| tab.id));
                }
            }
        }
        Self {
            ids: IdMinter::new(salt),
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

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    /// The workspace the desktop is the shared desktop of.
    const ROOM: &str = "0b7e7dfa-3ad5-4b43-9d6c-5f0d7c1f6a11";

    fn desktop(doc: &automerge::AutoCommit) -> SharedDesktop {
        shared_record::read(doc, None).unwrap()
    }

    fn strips(doc: &automerge::AutoCommit) -> Vec<String> {
        desktop(doc).tree.workspaces[0]
            .strips
            .iter()
            .map(|strip| strip.name.clone())
            .collect()
    }

    /// A workspace without a desktop starts one: the workspace itself, by its id, with its
    /// name and the listed strips.
    #[test]
    fn a_room_without_a_desktop_starts_with_the_listed_strips() {
        let mut doc = automerge::AutoCommit::new();
        assert!(settle_strips(&mut doc, ROOM, "Hangout", &names(&["Main", "Games"])).unwrap());
        assert_eq!(strips(&doc), ["Main", "Games"]);
        let tree: DesktopTree = desktop(&doc).tree;
        assert_eq!(tree.workspaces[0].name, "Hangout");
        assert_eq!(tree.workspaces[0].id().to_string(), ROOM);
        assert_eq!(tree.active, tree.workspaces[0].slot);
    }

    /// Settling again changes nothing; a newly listed strip is added after
    /// what is there, and the strips people made and who made them are kept.
    #[test]
    fn settling_adds_only_what_is_missing() {
        let mut doc = automerge::AutoCommit::new();
        settle_strips(&mut doc, ROOM, "Hangout", &names(&["Main"])).unwrap();
        let mut value = desktop(&doc);
        value.tree.workspaces[0]
            .strips
            .push(Strip::new(StripId(77), "Someone's"));
        value.authors.insert(77, "alice".into());
        shared_record::write(&mut doc, &value).unwrap();

        assert!(!settle_strips(&mut doc, ROOM, "Hangout", &names(&["Main"])).unwrap());
        assert!(settle_strips(&mut doc, ROOM, "Hangout", &names(&["Main", "Quiet"])).unwrap());
        assert_eq!(strips(&doc), ["Main", "Someone's", "Quiet"]);
        let value = desktop(&doc);
        assert_eq!(value.authors.get(&77).map(String::as_str), Some("alice"));
        let ids: std::collections::HashSet<_> = value.tree.workspaces[0]
            .strips
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids.len(), 3, "a new strip never reuses an id");
    }

    /// No listed strips leaves the record alone, even an absent one.
    #[test]
    fn no_strips_writes_nothing() {
        let mut doc = automerge::AutoCommit::new();
        assert!(!settle_strips(&mut doc, ROOM, "Hangout", &[]).unwrap());
        assert!(shared_record::read(&doc, None).is_none());
    }

    #[test]
    fn strip_names_are_trimmed_and_bounded() {
        assert_eq!(strip_name("  Games night ").unwrap(), "Games night");
        assert!(strip_name("   ").is_err());
        assert!(strip_name(&"x".repeat(49)).is_err());
        assert!(strip_name(&"é".repeat(48)).is_ok());
    }
}
