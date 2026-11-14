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
    // A workspace nobody named is held under the word the desk gives it, so a workspace asking for
    // that word is numbered beside it rather than standing as its twin.
    let taken = |candidate: &str| {
        held.iter()
            .any(|workspace| tilo_desktop_surface::names::workspace(&workspace.name) == candidate)
    };
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

    tilo_test_env::scenario! {
        fn a_new_workspaces_name_is_numbered_past_its_twins;

        phase a_second_workspace_of_one_name_is_numbered;
        {
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

        phase the_word_an_unnamed_workspace_reads_by_is_taken;
        // A desk whose one workspace nobody named still holds the word it reads by: a workspace the
        // person calls that is numbered beside it, never its twin.
        {
            crate::i18n::ensure();
            let tree = held(&[""]);
            assert_eq!(made(Some("Workspace"), &tree), "Workspace 2");
            assert_eq!(made(Some("Later"), &tree), "Later");
        }

        phase a_blank_name_is_the_next_numbered_one;
        {
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

        phase a_number_already_held_is_passed_by_and_a_free_one_is_taken;
        // The number a held name is given is the first one nobody holds: past the twins already
        // numbered, and into the gap one of them left.
        {
            let tree = held(&["Later", "Later 2", "Later 3"]);
            assert_eq!(
                made(Some("Later"), &tree),
                "Later 4",
                "every number held is passed by"
            );
            let gap = held(&["Later", "Later 3"]);
            assert_eq!(
                made(Some("Later"), &gap),
                "Later 2",
                "the first free number, not one past the highest"
            );
        }

        phase the_desks_own_number_starts_at_one_and_is_never_a_twin;
        // The name the desk gives by number is a name like any other: the first on a desk with
        // nothing on it, and numbered beside the workspace that already holds it.
        {
            crate::i18n::ensure();
            assert_eq!(
                made(None, &[]),
                "Workspace 1",
                "the first workspace of an empty desk"
            );
            assert_eq!(
                made(Some("Home"), &[]),
                "Home",
                "nothing is held, so the name asked for is free"
            );
            let tree = held(&["Home", "Workspace 3"]);
            assert_eq!(
                made(None, &tree),
                "Workspace 3 2",
                "the number the desk would give is held, so it is numbered beside that workspace"
            );
        }
    }
}
