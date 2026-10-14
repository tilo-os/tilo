//! The strips a persistent room's shared desktop has: the named places its
//! operator lists, so the first person in finds them in the top bar. The
//! desktop is the record members' pages keep their windows in (one JSON
//! value under `desktop`: the tree, the tabs' records and who made what).
//! A listed strip is added where its name is missing; everything people
//! made, strips and windows alike, is kept.

use automerge::{ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use tilo_desktop_model::{DesktopTree, IdMinter, Strip, StripId, Workspace, WorkspaceId};

/// The room record members' pages keep the shared desktop in.
pub(crate) const DESKTOP: (&str, &str) = ("so.tilo.browser-desktop", "desktop");
/// The key of the desktop's value in its record.
const DESKTOP_KEY: &str = "desktop";
/// The longest strip name, the same as one a person names in the top bar.
const STRIP_NAME_MAX: usize = 48;

/// `name` trimmed, as a strip's name.
///
/// # Errors
/// The name is empty or longer than a strip's name may be.
pub(crate) fn strip_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > STRIP_NAME_MAX {
        return Err(format!(
            "a strip name is 1-{STRIP_NAME_MAX} characters, not {name:?}"
        ));
    }
    Ok(name.to_owned())
}

/// Give the room's shared desktop a strip named each of `names`, in order,
/// in its first workspace where none has that name yet. A room with no
/// desktop starts one: a workspace named `shown` holding those strips.
/// Returns whether it changed the desktop.
///
/// # Errors
/// The record holds a desktop that is not one.
pub(crate) fn settle_strips(
    doc: &mut automerge::AutoCommit,
    room: &str,
    shown: &str,
    names: &[String],
) -> Result<bool, String> {
    if names.is_empty() {
        return Ok(false);
    }
    let mut value = match doc.get(ROOT, DESKTOP_KEY).map_err(|e| e.to_string())? {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Str(text) => serde_json::from_str::<serde_json::Value>(text)
                .map_err(|error| format!("read the desktop: {error}"))?,
            _ => return Err("the desktop is not a text value".into()),
        },
        Some(_) => return Err("the desktop is not a text value".into()),
        None => serde_json::json!({ "records": [], "authors": {} }),
    };
    let mut tree: DesktopTree = match value.get("tree") {
        Some(tree) => serde_json::from_value(tree.clone())
            .map_err(|error| format!("read the desktop: {error}"))?,
        None => DesktopTree::default(),
    };
    let mut minter = Minter::new(room, &tree);
    if tree.workspaces.is_empty() {
        let workspace = Workspace::new(WorkspaceId(minter.mint()), shown);
        tree.active = workspace.id;
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
    let workspace_id = workspace.id;
    let first = workspace.strips.first().map(|strip| strip.id);
    if let Some(first) = first {
        tree.active_strip.entry(workspace_id).or_insert(first);
    }
    let tree = serde_json::to_value(&tree).map_err(|error| error.to_string())?;
    if let Some(object) = value.as_object_mut() {
        object.insert("tree".into(), tree);
    } else {
        return Err("the desktop is not an object".into());
    }
    doc.put(ROOT, DESKTOP_KEY, value.to_string())
        .map_err(|error| error.to_string())?;
    Ok(true)
}

/// Ids for the strips the room adds: under a namespace of the room's own,
/// never one the desktop already holds.
struct Minter {
    ids: IdMinter,
    taken: std::collections::HashSet<u64>,
}

impl Minter {
    fn new(room: &str, tree: &DesktopTree) -> Self {
        let hash = blake3::hash(format!("room strips {room}").as_bytes());
        let [a, b, ..] = *hash.as_bytes();
        // Salt 0 is no namespace at all.
        let salt = u16::from_le_bytes([a, b]).max(1);
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

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    fn desktop(doc: &automerge::AutoCommit) -> serde_json::Value {
        match doc.get(ROOT, DESKTOP_KEY).unwrap().unwrap() {
            (Value::Scalar(scalar), _) => match scalar.as_ref() {
                ScalarValue::Str(text) => serde_json::from_str(text).unwrap(),
                _ => panic!("not text"),
            },
            _ => panic!("not a scalar"),
        }
    }

    fn strips(doc: &automerge::AutoCommit) -> Vec<String> {
        let tree: DesktopTree = serde_json::from_value(desktop(doc)["tree"].clone()).unwrap();
        tree.workspaces[0]
            .strips
            .iter()
            .map(|strip| strip.name.clone())
            .collect()
    }

    /// A room without a desktop starts one: a workspace with the room's
    /// name and the listed strips, the first one active, and the parts
    /// members' pages read (records, authors) present.
    #[test]
    fn a_room_without_a_desktop_starts_with_the_listed_strips() {
        let mut doc = automerge::AutoCommit::new();
        assert!(settle_strips(&mut doc, "room", "Hangout", &names(&["Main", "Games"])).unwrap());
        assert_eq!(strips(&doc), ["Main", "Games"]);
        let value = desktop(&doc);
        assert_eq!(value["records"], serde_json::json!([]));
        let tree: DesktopTree = serde_json::from_value(value["tree"].clone()).unwrap();
        assert_eq!(tree.workspaces[0].name, "Hangout");
        assert_eq!(tree.active, tree.workspaces[0].id);
        assert_eq!(
            tree.active_strip.get(&tree.active),
            Some(&tree.workspaces[0].strips[0].id)
        );
    }

    /// Settling again changes nothing; a newly listed strip is added after
    /// what is there, and the strips people made and their windows' records
    /// are kept.
    #[test]
    fn settling_adds_only_what_is_missing() {
        let mut doc = automerge::AutoCommit::new();
        settle_strips(&mut doc, "room", "Hangout", &names(&["Main"])).unwrap();
        let mut value = desktop(&doc);
        let mut tree: DesktopTree = serde_json::from_value(value["tree"].clone()).unwrap();
        tree.workspaces[0]
            .strips
            .push(Strip::new(StripId(77), "Someone's"));
        value["tree"] = serde_json::to_value(&tree).unwrap();
        value["records"] = serde_json::json!([[5, "rec"]]);
        value["authors"] = serde_json::json!({ "77": "alice" });
        doc.put(ROOT, DESKTOP_KEY, value.to_string()).unwrap();

        assert!(!settle_strips(&mut doc, "room", "Hangout", &names(&["Main"])).unwrap());
        assert!(settle_strips(&mut doc, "room", "Hangout", &names(&["Main", "Quiet"])).unwrap());
        assert_eq!(strips(&doc), ["Main", "Someone's", "Quiet"]);
        let value = desktop(&doc);
        assert_eq!(value["records"], serde_json::json!([[5, "rec"]]));
        assert_eq!(value["authors"]["77"], "alice");
        let tree: DesktopTree = serde_json::from_value(value["tree"].clone()).unwrap();
        let ids: std::collections::HashSet<_> =
            tree.workspaces[0].strips.iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), 3, "a new strip never reuses an id");
    }

    /// No listed strips leaves the record alone, even an absent one.
    #[test]
    fn no_strips_writes_nothing() {
        let mut doc = automerge::AutoCommit::new();
        assert!(!settle_strips(&mut doc, "room", "Hangout", &[]).unwrap());
        assert!(doc.get(ROOT, DESKTOP_KEY).unwrap().is_none());
    }

    #[test]
    fn strip_names_are_trimmed_and_bounded() {
        assert_eq!(strip_name("  Games night ").unwrap(), "Games night");
        assert!(strip_name("   ").is_err());
        assert!(strip_name(&"x".repeat(49)).is_err());
        assert!(strip_name(&"é".repeat(48)).is_ok());
    }
}
