//! Who is here, as every surface that names people reads them: what each member of the workspace
//! is called, where someone is when no window here shows their record, and who wrote the line
//! about them. The shell draws it.

use std::collections::HashMap;

use collab_types::MemberId;
use tilo_desktop_model::Layout;

/// What each member in the active workspace is called, with this person by their own name: the one
/// source every surface that names people reads, so a name is spelt and cased the same in People,
/// the roster and settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeopleNames(HashMap<MemberId, String>);

impl PeopleNames {
    /// The roster's names for the active workspace of `desktop`, as `local_user` (named
    /// `own_name`) sees it.
    #[must_use]
    pub fn of(desktop: &Layout, local_user: &str, own_name: Option<&str>) -> Self {
        let Some(workspace) = desktop.active_workspace() else {
            return Self::default();
        };
        let mut names: HashMap<MemberId, String> = workspace
            .members
            .iter()
            .filter(|member| !member.name.trim().is_empty())
            .map(|member| (member.id.clone(), member.name.clone()))
            .collect();
        if let Some(own) = own_name.map(str::trim).filter(|own| !own.is_empty()) {
            names.insert(MemberId::person(local_user), own.to_owned());
        }
        Self(names)
    }

    /// What the member `id` is called, when the roster names them.
    #[must_use]
    pub fn get(&self, id: &MemberId) -> Option<&str> {
        self.0.get(id).map(String::as_str)
    }
}

/// Where someone is when no window here shows it: "In Notes".
pub fn in_app(app: &str) -> String {
    crate::desktop::i18n::t_args("people-panel-in-app", &[("app", app.to_owned().into())])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterStatus {
    pub text: String,
    pub source: LineSource,
}

/// Who wrote a line shown about someone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineSource {
    /// The person typed it.
    Written,
    /// A model (or another generator) wrote it from what they did.
    Generated,
    /// Plainly read off what they did ("Editing Welcome in Notes").
    Observed,
}
