//! What the gallery app (`apps/tl/gallery`) shows of the toolkit's described components. The gallery is
//! the window a person opens to see every component of the toolkit as tilolang places it, and a
//! component described in this crate is shown there the day it is written: a described component no
//! view of the gallery names is a gap.

use super::catalogue;

/// Whether `source` names the component `name` as `ui.<name>`, not as the start of a longer name
/// (`ui.Card` is not `ui.CardGrid`).
#[must_use]
pub fn names(source: &str, name: &str) -> bool {
    let wanted = format!("ui.{name}");
    source.match_indices(&wanted).any(|(at, _)| {
        !source
            .get(at.saturating_add(wanted.len())..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// What the gallery's sources, all together, show of the toolkit: the described components no view of
/// theirs names, and those they show although the component says no window can.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    /// The described components no view of the gallery names, in catalogue order.
    pub missing: Vec<String>,
    /// The components whose declaration says the gallery cannot show them, which it shows.
    pub shown_but_exempt: Vec<String>,
}

impl Coverage {
    /// What to change, a sentence per kind of gap: empty when the gallery shows every described
    /// component and contradicts no declaration.
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if !self.missing.is_empty() {
            problems.push(format!(
                "the gallery (apps/tl/gallery) shows no `ui.{{Name}}` for: {}. Show it there, \
                 or say at its declaration why the gallery cannot \
                 (`#[component(not_in_gallery = \"…\")]` on its props).",
                self.missing.join(", ")
            ));
        }
        if !self.shown_but_exempt.is_empty() {
            problems.push(format!(
                "these say the gallery cannot show them, and the gallery shows them: take \
                 `not_in_gallery` off their declaration, or stop showing them: {}",
                self.shown_but_exempt.join(", ")
            ));
        }
        problems
    }
}

/// What the gallery's tilolang sources, together, show of the toolkit's described components.
#[must_use]
pub fn coverage(source: &str) -> Coverage {
    let catalogue = catalogue();
    let mut missing = Vec::new();
    let mut shown_but_exempt = Vec::new();
    for name in catalogue.names() {
        // Shown nowhere, and its declaration says why the gallery cannot show it: not a gap. A
        // component marked so *and* shown is not a gap either (a view may still place it), but
        // a marker on a component the gallery does show would be a lie.
        let exempt = catalogue
            .get(name)
            .is_some_and(|component| component.semantics.not_in_gallery.is_some());
        match (names(source, name), exempt) {
            (false, false) => missing.push(name.to_owned()),
            (true, true) => shown_but_exempt.push(name.to_owned()),
            _ => {}
        }
    }
    Coverage {
        missing,
        shown_but_exempt,
    }
}
