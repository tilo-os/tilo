//! Which action a name asks for anywhere under the resource a path names: a
//! published action anywhere under it is the last resort.

use super::*;

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
