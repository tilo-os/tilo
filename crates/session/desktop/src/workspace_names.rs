//! What a workspace made here is called.

use tilo_desktop_model::Workspace;

/// The name a workspace the desk is making gets: `name` as the person gave it, else the next number
/// after those `held` here. A name already held is numbered, so two workspaces of one name never
/// stand side by side in the bar, the menu and the overview, where a person picks one by its name.
pub(crate) fn made(name: Option<&str>, held: &[Workspace]) -> String {
    let wanted = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| numbered(held), str::to_owned);
    let taken = |candidate: &str| held.iter().any(|workspace| workspace.name == candidate);
    if !taken(&wanted) {
        return wanted;
    }
    // Each number names one more candidate, so fewer of them are held than there are numbers: the
    // first free one is found.
    let mut number: usize = 2;
    loop {
        let candidate = format!("{wanted} {number}");
        if !taken(&candidate) {
            return candidate;
        }
        number = number.saturating_add(1);
    }
}

/// The name of a workspace the desk numbers itself: the next one after those `held` here.
fn numbered(held: &[Workspace]) -> String {
    tilo_desktop_surface::i18n::t_args(
        "browser-new-workspace-name",
        &[("number", held.len().saturating_add(1).into())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::WorkspaceSlot;

    /// `names` held as workspaces here, the way a desk keeps them.
    fn held(names: &[&str]) -> Vec<Workspace> {
        names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let slot = u64::try_from(index.saturating_add(1)).expect("a small index");
                Workspace::new(WorkspaceSlot(slot), *name)
            })
            .collect()
    }

    #[test]
    fn a_second_workspace_of_one_name_is_numbered() {
        let tree = held(&["Home", "Later"]);
        assert_eq!(
            made(Some("Later"), &tree),
            "Later 2",
            "a name already held is numbered, so the two can be told apart"
        );
        assert_eq!(
            made(Some(" Later "), &tree),
            "Later 2",
            "the name as it would be stored decides it"
        );
        assert_eq!(
            made(Some("Reading"), &tree),
            "Reading",
            "a name nobody holds is the name asked for"
        );
    }

    #[test]
    fn a_blank_name_is_the_next_numbered_one() {
        let tree = held(&["Home"]);
        assert_eq!(
            made(None, &tree),
            "Workspace 2",
            "a workspace made without a name is numbered by the desk"
        );
        assert_eq!(
            made(Some("   "), &tree),
            "Workspace 2",
            "a blank name is no name"
        );
    }
}
