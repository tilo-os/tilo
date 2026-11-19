//! Which node a segment of a path names, and which action a name asks for:
//! by rank, with a published action anywhere under the resource the last
//! resort.

use super::*;

/// Whether `segment`, a segment of a path, is written the way a program types a type
/// (`tasks.Task`): a package's alias, a dot, and a name that begins with a capital letter.
/// Nothing else is asked of the task's programs: a title, a key or a kind (`notes.notebook`)
/// is never a program form, so a path that reads one does not start a task's host.
pub(super) fn is_program_form(segment: &str) -> bool {
    segment.rsplit_once('.').is_some_and(|(alias, name)| {
        !alias.is_empty() && name.starts_with(|c: char| c.is_ascii_uppercase())
    })
}

/// How well `name`, a segment of a path, names `node`: [`Node::answers_to`]'s rank, and a type
/// named the way a program types it (`tasks.Task`) is rank 2 too, as its kind is: a listing
/// names a resource by that program form, so a name read off a row walks back to the resource.
pub(super) fn rank(name: &str, node: &Node, forms: Option<&ProgramForms>) -> Option<u8> {
    node.answers_to(name).or_else(|| {
        forms
            .is_some_and(|forms| forms.answers(name, &node.kind))
            .then_some(2)
    })
}

/// Pick the node `name` means among `nodes`: the best-ranked one, when one
/// is. Several equally good is an error naming them.
pub(super) fn pick<'a>(
    name: &str,
    nodes: &'a [Node],
    forms: Option<&ProgramForms>,
) -> Result<Option<&'a Node>, String> {
    let ranked: Vec<(u8, &Node)> = nodes
        .iter()
        .filter_map(|node| Some((rank(name, node, forms)?, node)))
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
pub(super) fn pick_action<'a>(
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

/// The actions published on `nodes`, each with the path below the app that names the
/// resource it is on, and on what each of the `held` containers holds.
pub(super) fn published_on(
    nodes: &[Node],
    held: &[(Node, Vec<Node>)],
    levels: &Levels,
) -> Vec<(String, ActionDescriptor)> {
    let mut published = Vec::new();
    for node in nodes {
        let path = path_of(node, levels).unwrap_or_else(|| node.name(nodes));
        for action in &node.actions {
            published.push((path.clone(), action.clone()));
        }
        let Some((_, inside)) = held.iter().find(|(container, _)| container.id == node.id) else {
            continue;
        };
        for child in inside {
            let at = format!("{path}/{}", child.name(inside));
            for action in &child.actions {
                published.push((at.clone(), action.clone()));
            }
        }
    }
    published
}

/// The one action among `published` whose id is `name`, with its path; `None` when none is or
/// several resources offer one.
pub(super) fn only_by_id<'a>(
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

/// The names below an app that reach `node`, in the levels this call read
/// (`Notebook/Todo`), or `None` when they do not reach it.
fn path_of(node: &Node, levels: &Levels) -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    let mut current = node.clone();
    for _ in 0..DEEP_READS {
        let level = levels
            .values()
            .find(|level| level.children.iter().any(|seen| seen.id == current.id))?;
        names.push(current.name(&level.children));
        if current.parent.is_empty() {
            names.reverse();
            return Some(names.join("/"));
        }
        current = levels
            .values()
            .flat_map(|level| level.children.iter())
            .find(|seen| seen.id == current.parent)?
            .clone();
    }
    None
}

/// The action `name` means anywhere in what `resolved` names: the target's
/// own actions (`notes/Todo#rename`), else one the app publishes on a
/// resource of its own (`settings#add-email-account` is on `settings.root`).
/// `ranked` is what the app answered for `name`, each action with how well it
/// is named and the key of the resource it is published on. Several
/// resources carrying it equally well is refused naming them, each with the
/// path that names it: the app alone does not mean one of two.
pub(super) fn pick_action_anywhere<'a>(
    name: &str,
    resolved: &'a Resolved,
    ranked: &[(u8, String, &'a ActionDescriptor)],
) -> Result<(Option<String>, &'a ActionDescriptor), String> {
    let error = match pick_action(name, &resolved.actions, &resolved.path) {
        Ok(action) => return Ok((None, action)),
        Err(error) => error,
    };
    // An action some way below what the path names is not that resource's:
    // a note's own action is the note's, not the notebook's. The app is the
    // one name that stands for what it publishes anywhere in its directory.
    if resolved.node.is_some() {
        return Err(error);
    }
    let Some(best) = ranked.iter().map(|(rank, ..)| *rank).min() else {
        return Err(error);
    };
    let mut found: Vec<(String, &'a ActionDescriptor)> = Vec::new();
    for (_, key, action) in ranked.iter().filter(|(rank, ..)| *rank == best) {
        if !found.iter().any(|(_, seen)| seen.address == action.address) {
            found.push((key.clone(), action));
        }
    }
    match found.as_slice() {
        [(key, only)] => Ok((Some(key.clone()), only)),
        // A person is told the few that may be meant, not every action of an
        // app that happens to share a word with the one asked for.
        many => Err(format!(
            "{name} names {} actions in {}: {}; name one with the resource it is on",
            many.len(),
            resolved.path,
            many.iter()
                .take(CLOSE_MATCHES)
                .map(|(key, action)| format!("{}/{}#{}", resolved.path, key, action.address.action))
                .chain(
                    (many.len() > CLOSE_MATCHES)
                        .then(|| format!("… +{}", many.len().saturating_sub(CLOSE_MATCHES)))
                )
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// How well `name` names `action`: its id, its label, the start of its id,
/// or not at all.
pub(super) fn action_rank(name: &str, action: &ActionDescriptor) -> Option<u8> {
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
pub(super) fn named_beyond_its_start(
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
pub(super) fn reads_action(actions: &[ActionDescriptor]) -> Option<ActionDescriptor> {
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
