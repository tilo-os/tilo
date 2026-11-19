//! Which kinds of resource a person looks for: the window's own controls and
//! the operations of the machine it runs on are published to be addressed,
//! never listed.

/// Whether `kind` names a control the window already draws, or an
/// operation of the machine's: nodes published so a caller can
/// address them, never something a person looks for in a list.
#[must_use]
pub fn kind_mirrors_a_control(kind: &str) -> bool {
    kind == "tilo.control"
        || kind == "tl.host"
        || ["tilo.control.", "tilo.ui.", "tilo.host."]
            .iter()
            .any(|prefix| kind.starts_with(prefix))
}

/// Whether `kind` names a control the window draws: its press, drag, focus
/// or value stands in for the pointer and keyboard of the person at the
/// device. Only that person invokes an action on it; anyone else, an agent
/// above all, is refused it and never offered it. What a control does that
/// others need, the app offers as an action of its own.
#[must_use]
pub fn kind_is_a_drawn_control(kind: &str) -> bool {
    kind == "tilo.control"
        || ["tilo.control.", "tilo.ui."]
            .iter()
            .any(|prefix| kind.starts_with(prefix))
}

/// Whether a resource of `kind` titled `label` is something a person looks
/// for: titled, and the app's own rather than a mirror of one of its
/// controls or an operation of the machine's. Those are published to
/// be addressed, never listed.
#[must_use]
pub fn listed_for_people(kind: &str, label: &str) -> bool {
    !label.trim().is_empty() && !kind_mirrors_a_control(kind)
}
