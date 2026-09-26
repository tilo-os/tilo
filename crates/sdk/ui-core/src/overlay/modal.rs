use super::scrim::Scrim;
use crate::a11y::{AccessNode, AccessState, Role};

/// A modal overlay entry: a dimming scrim plus an optional title that names
/// the dialog for assistive tech. A modal traps keyboard focus while it's up
/// (enforced by the host) and announces its title when it opens.
#[derive(Clone, Debug)]
pub struct Modal {
    pub id_tag: String,
    pub scrim: Scrim,
    /// Empty leaves the dialog unnamed.
    pub title: String,
}

impl Modal {
    pub fn new(id_tag: &str) -> Self {
        Self {
            id_tag: id_tag.into(),
            scrim: Scrim::dark(),
            title: String::new(),
        }
    }
    #[must_use]
    pub fn with_scrim(mut self, s: Scrim) -> Self {
        self.scrim = s;
        self
    }

    /// Set the dialog title used as the accessible name announced on open.
    #[must_use]
    pub fn with_title(mut self, title: &str) -> Self {
        self.title = title.into();
        self
    }

    /// The modal's semantics: a [`Role::Dialog`] marked modal, named by its
    /// title (FTL-resolved). Hosts surface this so an AT announces the dialog
    /// the moment it appears.
    pub fn accessibility(&self) -> AccessNode {
        let title = crate::i18n!(&self.title);
        let name = (!title.is_empty()).then_some(title);
        AccessNode::new(Role::Dialog)
            .with_optional_name(name)
            .with_state(AccessState {
                modal: true,
                ..Default::default()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modal_reports_the_dialog_role_modal_state_and_its_title_if_any() {
        // phase: modal_reports_dialog_role_modal_and_title
        {
            let m = Modal::new("confirm").with_title("Delete file?");
            let node = m.accessibility();
            assert_eq!(node.role, Role::Dialog);
            assert_eq!(node.name.as_deref(), Some("Delete file?"));
            assert!(
                node.state.modal,
                "a modal overlay traps focus → modal flag set"
            );
        }
        // phase: untitled_modal_has_no_accessible_name
        {
            let m = Modal::new("confirm");
            assert_eq!(m.accessibility().name, None);
        }
    }
}
