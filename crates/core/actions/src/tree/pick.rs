//! Which node a segment of an address names, and which action a name asks
//! for: by rank, the best one, or an error naming the several it could be.

use super::{CLOSE_MATCHES, NAMES_LISTED, Node, close_matches};
use crate::{ActionDescriptor, OperationKind};

/// The names programs give the types of resources: what tells
/// whether a segment written the way a program types a type (`tasks.Task`)
/// names a resource of `resource_type`.
pub trait TypeNames: Send + Sync {
    /// Whether `name` is what a program calls `resource_type`.
    fn calls(&self, name: &str, resource_type: &str) -> bool;
}

/// Whether `segment`, a segment of a path, is written the way a program types a type
/// (`tasks.Task`): a package's alias, a dot, and a name that begins with a capital letter.
/// Nothing else is asked of the task's programs: a title, a key or a kind (`notes.notebook`)
/// is never a program form, so a path that reads one does not start a task's host.
pub fn is_program_form(segment: &str) -> bool {
    segment.rsplit_once('.').is_some_and(|(alias, name)| {
        !alias.is_empty() && name.starts_with(|c: char| c.is_ascii_uppercase())
    })
}

/// How well `name`, a segment of a path, names `node`: [`Node::answers_to`]'s rank, and a type
/// named the way a program types it (`tasks.Task`) is rank 2 too, as its resource type is: a listing
/// names a resource by that program form, so a name read off a row walks back to the resource.
pub fn rank(name: &str, node: &Node, types: Option<&dyn TypeNames>) -> Option<u8> {
    node.answers_to(name).or_else(|| {
        types
            .is_some_and(|types| types.calls(name, &node.resource_type))
            .then_some(2)
    })
}

/// Pick the node `name` means among `nodes`: the best-ranked one, when one
/// is. Several equally good is an error naming them.
pub fn pick<'a>(
    name: &str,
    nodes: &'a [Node],
    types: Option<&dyn TypeNames>,
) -> Result<Option<&'a Node>, String> {
    let ranked: Vec<(u8, &Node)> = nodes
        .iter()
        .filter_map(|node| Some((rank(name, node, types)?, node)))
        .collect();
    let Some(best) = ranked.iter().map(|(rank, _)| *rank).min() else {
        return Ok(None);
    };
    let mut found: Vec<&Node> = Vec::new();
    for (_, node) in ranked.iter().filter(|(rank, _)| *rank == best) {
        if !found.iter().any(|seen| seen.id == node.id) {
            found.push(node);
        }
    }
    // One that owns the rest (their keys name it) is the one meant: a note,
    // not the body published beside it under the same title.
    let owners: Vec<&&Node> = found
        .iter()
        .filter(|node| {
            found
                .iter()
                .all(|other| other.id == node.id || node.owns(other))
        })
        .collect();
    if let [owner] = owners.as_slice() {
        return Ok(Some(**owner));
    }
    match found.as_slice() {
        [one] => Ok(Some(one)),
        many => Err(format!(
            "{name} names {} resources: {}; name one by its key",
            many.len(),
            many.iter()
                .take(NAMES_LISTED)
                .map(|node| format!("{} ({})", node.label.trim(), node.key))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The action `name` means among `actions`: its id, its label, or the one id
/// that starts with it.
pub fn pick_action<'a>(
    name: &str,
    actions: &'a [ActionDescriptor],
    at: &str,
) -> Result<&'a ActionDescriptor, String> {
    let id = |action: &ActionDescriptor| action.address.action.clone();
    if let Some(found) = actions
        .iter()
        .find(|action| id(action).eq_ignore_ascii_case(name))
        .or_else(|| {
            actions
                .iter()
                .find(|action| action.action_label.eq_ignore_ascii_case(name))
        })
    {
        return Ok(found);
    }
    let lower = name.to_lowercase();
    let mut starts = actions
        .iter()
        .filter(|action| id(action).to_lowercase().starts_with(&lower));
    if let (Some(only), None) = (starts.next(), starts.next()) {
        return Ok(only);
    }
    let mut ids: Vec<String> = actions.iter().map(id).collect();
    ids.dedup();
    // What the caller may have meant, not everything there is: an app with
    // ninety actions is not told as one wall of names.
    let close = close_matches(name, &ids, CLOSE_MATCHES);
    // An empty list is what this caller is offered here, never the app's whole: what the app
    // offers deeper, or keeps for the person at its device, is not in it.
    Err(if close.is_empty() {
        format!("{at} has no action {name}")
    } else {
        format!(
            "{at} has no action {name}; close matches: {}",
            close.join(", ")
        )
    })
}

/// The one action among `published` whose id is `name`, with its path; `None` when none is or
/// several resources offer one.
pub fn only_by_id<'a>(
    name: &str,
    published: &'a [(String, ActionDescriptor)],
) -> Option<(&'a str, &'a ActionDescriptor)> {
    let mut by_id = published
        .iter()
        .filter(|(_, action)| action_rank(name, action) == Some(0));
    let (path, first) = by_id.next()?;
    by_id
        .all(|(_, other)| other.address == first.address)
        .then_some((path.as_str(), first))
}

/// How well `name` names `action`: its id, its label, the start of its id,
/// or not at all.
pub fn action_rank(name: &str, action: &ActionDescriptor) -> Option<u8> {
    if action.address.action.eq_ignore_ascii_case(name) {
        return Some(0);
    }
    if action.action_label.eq_ignore_ascii_case(name) {
        return Some(1);
    }
    action
        .address
        .action
        .to_lowercase()
        .starts_with(&name.to_lowercase())
        .then_some(2)
}

/// `picked`, the action `name` was found to mean at `path`, unless `name` is
/// the word that starts an app (`starts`) and names the action by nothing
/// but the start of its id: `open` begins `open-link` and is neither its id
/// nor its label, so it is not that action.
pub fn named_beyond_its_start(
    starts: bool,
    name: &str,
    path: &str,
    picked: Result<(String, ActionDescriptor), String>,
) -> Result<(String, ActionDescriptor), String> {
    match picked {
        Ok((_, found)) if starts && action_rank(name, &found) == Some(2) => Err(format!(
            "{path} has no action {name}; its {} is another action",
            found.address.action
        )),
        picked => picked,
    }
}

/// The action that reads a resource, among those its directory level offers:
/// the one declared to read its document, else one named `read`, else a view
/// whose action starts with `read` (`read-settings` next to `read`).
pub fn reads_action(actions: &[ActionDescriptor]) -> Option<ActionDescriptor> {
    actions
        .iter()
        .find(|action| action.contract.reads_document())
        .or_else(|| {
            actions
                .iter()
                .find(|action| action.address.action == "read")
        })
        .or_else(|| {
            actions.iter().find(|action| {
                action.kind == OperationKind::View && action.address.action.starts_with("read")
            })
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::{Node, TypeNames, pick};

    fn page(id: &str, label: &str, parent: &str, ordinal: usize) -> Node {
        Node {
            id: format!("notes.page:{id}"),
            resource_type: "notes.page".into(),
            label: label.into(),
            key: format!("notes.page:{id}"),
            parent: parent.into(),
            ordinal,
            container: false,
            actions: Vec::new(),
        }
    }

    #[test]
    fn a_repeated_title_is_named_by_its_place_and_the_path_takes_it() {
        // Two pages titled alike are named by their places, and that name is a
        // path back to each: the first keeps the title, the second its place.
        let nodes = vec![
            page("aaaaaaaa", "TODO", "notes.pages", 1),
            page("bbbbbbbb", "TODO", "notes.pages", 2),
        ];
        assert_eq!(nodes[0].name(&nodes), "TODO");
        assert_eq!(nodes[1].name(&nodes), "TODO 2");
        let id = |name: &str| {
            pick(name, &nodes, None)
                .unwrap()
                .map(|found| found.id.clone())
        };
        assert_eq!(id("TODO"), Some("notes.page:aaaaaaaa".into()));
        assert_eq!(id("TODO 2"), Some("notes.page:bbbbbbbb".into()));
        assert_eq!(id("TODO 3"), None);
    }

    #[test]
    fn a_title_that_names_several_offers_the_keys_that_tell_them_apart() {
        // A title several resources share is refused with the key each one goes by:
        // the key is a name the next path can use.
        let nodes = vec![
            page("aaaaaaaa", "README", "notes.notebook", 1),
            page("bbbbbbbb", "README", "notes.notebook", 1),
        ];
        let error = pick("README", &nodes, None).unwrap_err();
        assert_eq!(
            error,
            "README names 2 resources: README (notes.page:aaaaaaaa), README (notes.page:bbbbbbbb); \
         name one by its key"
        );
    }

    #[test]
    fn a_type_written_as_a_program_types_it_names_its_resources() {
        struct Pages;
        impl TypeNames for Pages {
            fn calls(&self, name: &str, resource_type: &str) -> bool {
                resource_type == "notes.page" && name == "notes.Note"
            }
        }
        let nodes = vec![page("aaaaaaaa", "Todo", "notes.pages", 1)];
        assert!(pick("notes.Note", &nodes, None).unwrap().is_none());
        let found = pick("notes.Note", &nodes, Some(&Pages)).unwrap();
        assert_eq!(
            found.map(|node| node.id.as_str()),
            Some("notes.page:aaaaaaaa")
        );
    }
}
