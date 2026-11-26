//! A workspace's shared desktop as one record of its store: the kinds a store declares it by,
//! the founding every copy of it derives, and the document a device starts writing it on.
//!
//! The record keeps its workspaces, strips and windows in a map each
//! ([`crate::shared_resource`]), and every copy founds it with the same change
//! ([`founding`]), so a desktop another device founded and one this device starts as it first
//! opens a window are one record.

use automerge::AutoCommit;
use tilo_actions::surface::{ReaderRule, Surface, content_verb, map_field};

use crate::ownership::{DESKTOP_DOCUMENT, DESKTOP_KIND};

/// The maps the record keeps its content in, at its root, by key.
const CONTAINERS: [(&str, &str); 3] = [
    ("workspaces", "Workspaces"),
    ("strips", "Strips"),
    ("windows", "Windows"),
];

/// The verbs an arranging action of the shell names its changes by: every change of a layout or of
/// a person's view of it the store is told is one of them, or an edit.
pub const ARRANGING_VERBS: [&str; 16] = [
    "move",
    "resize",
    "maximize-strip",
    "close",
    "activate",
    "pin",
    "position",
    "size",
    "drawn",
    "drop-row",
    "place",
    "open",
    "float",
    "snap-back",
    "show",
    "retry",
];

/// The kind of a window's view: what it showed and where it was, kept for the person whose window
/// it is.
pub const WINDOW_VIEW_KIND: &str = "desktop.window-view";

/// The kind the store declares a workspace's shared desktop by.
#[must_use]
pub fn layout() -> Surface {
    declaring(DESKTOP_KIND, "Desktop", None, CONTAINERS, true)
}

/// The kind the store declares a person's view of a layout by: read by the person who made it.
#[must_use]
pub fn person_view() -> Surface {
    declaring(
        tilo_action_policy::roles::PERSON_VIEW_KIND,
        "View",
        Some(ReaderRule::Author { until: None }),
        [],
        true,
    )
}

/// The kind the store declares a window's view by: read by the person whose window it is.
#[must_use]
pub fn window_view() -> Surface {
    declaring(
        WINDOW_VIEW_KIND,
        "Window view",
        Some(ReaderRule::Author { until: None }),
        [],
        false,
    )
}

/// The kind `kind` as a store declares it: an edit and the arranging actions a shell's changes
/// its fields keep their items in.
fn declaring<const N: usize>(
    kind: &str,
    label: &str,
    readers: Option<ReaderRule>,
    containers: [(&str, &str); N],
    arranging: bool,
) -> Surface {
    let mut verbs = vec![content_verb(tilo_actions::stamp::Stamp::TYPE_VERB, "Edit")];
    if arranging {
        verbs.extend(
            ARRANGING_VERBS
                .iter()
                .map(|named| content_verb(named, named)),
        );
    }
    Surface {
        kind: kind.to_owned(),
        label: label.to_owned(),
        version: 1,
        person: false,
        readers,
        verbs,
        fields: containers
            .iter()
            .map(|(path, title)| map_field(path, "desktop", title))
            .collect(),
        types: Vec::new(),
        shows: Vec::new(),
        live: None,
        setters: false,
    }
}

/// The change that founds `record`, `workspace`'s shared desktop, every copy of it alike: the
/// maps its entries are kept in, and nothing else. Another workspace's desktop is founded by a
/// change of its own, so the two never share one.
///
/// # Errors
/// A container cannot be made.
pub fn founding(
    workspace: &str,
    record: &str,
) -> Result<automerge::Change, automerge::AutomergeError> {
    let containers = collab_types::document::founding::of_kind(&layout());
    let scope = collab_types::document::founding::Scope {
        workspace,
        kind: DESKTOP_KIND,
        id: record,
    };
    collab_types::document::founding::structure(scope, &containers)
}

/// A document of `workspace`'s shared desktop, founded with the change every copy of it
/// derives, ready for the first content a device writes of it. [`found_into`] founds one already
/// open.
///
/// # Errors
/// The founding cannot be made, or the document refused it.
pub fn founded(workspace: &str) -> Result<AutoCommit, String> {
    let mut doc = AutoCommit::new();
    found_into(workspace, &mut doc)?;
    Ok(doc)
}

/// Found `doc`, `workspace`'s shared desktop, with the change every copy of the record derives:
/// the maps its entries are kept in.
///
/// # Errors
/// The founding cannot be made, or the document refused it.
pub fn found_into(workspace: &str, doc: &mut AutoCommit) -> Result<(), String> {
    use tilo_automerge::ApplyIncoming as _;
    let founding = founding(workspace, DESKTOP_DOCUMENT).map_err(|error| error.to_string())?;
    doc.apply_incoming([founding])
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "workspace";

    /// Two devices starting a workspace's desktop from the same founding write one record: the
    /// change each derives is the same, so what either puts in a container the other keeps.
    #[test]
    fn every_copy_derives_the_one_founding() {
        let ours = founding(WORKSPACE, DESKTOP_DOCUMENT).expect("a desktop founds");
        let theirs = founding(WORKSPACE, DESKTOP_DOCUMENT).expect("a desktop founds");
        assert_eq!(ours.hash(), theirs.hash());
        let elsewhere = founding("another", DESKTOP_DOCUMENT).expect("a desktop founds");
        assert_ne!(
            ours.hash(),
            elsewhere.hash(),
            "another workspace's desktop is founded by a change of its own"
        );
        let containers: Vec<String> = collab_types::document::founding::of_kind(&layout())
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(containers, ["strips", "windows", "workspaces"]);
    }
}
