//! What one search reaches: the one scope vocabulary, from the narrowest to the widest.
//! Everything that opens search names one [`Scope`] and nothing else: a chord, a window
//! group's new-tab control, an app's own search button, a launcher.

use serde::{Deserialize, Serialize};

use tilo_resource::ScopeDecl;

/// One window group's identity, as the desk numbers it.
pub type WindowGroupId = u64;

/// One app's own search: the app, and the part of it a control asked for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<ScopeDecl>,
}

impl AppScope {
    /// Everything `app` publishes.
    #[must_use]
    pub fn of(app: impl Into<String>) -> Self {
        Self {
            app: Some(app.into()),
            part: None,
        }
    }

    /// The app in front, whichever the host shows there.
    #[must_use]
    pub fn front() -> Self {
        Self::default()
    }

    /// This app, narrowed to `part` of it.
    #[must_use]
    pub fn within(self, part: ScopeDecl) -> Self {
        Self {
            part: Some(part),
            ..self
        }
    }

    /// The same app, as `app` names it.
    #[must_use]
    pub fn named(mut self, app: impl Into<String>) -> Self {
        self.app = Some(app.into());
        self
    }
}

/// How far one search reaches, from narrowest to widest: the one vocabulary every control
/// that opens search asks in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// One app's own: what it publishes, everything or the part of it a control named.
    /// The app's own search, which is never the workspace's.
    InApp(AppScope),
    WindowGroup(Option<WindowGroupId>),
    #[default]
    Workspace,
    Everywhere,
}

impl Scope {
    /// The group a hit opens in, when the search is one window group's.
    #[must_use]
    pub fn window_group(&self) -> Option<WindowGroupId> {
        match self {
            Self::WindowGroup(group) => *group,
            _ => None,
        }
    }

    /// Whether the search reaches past the workspace in front.
    #[must_use]
    pub fn everywhere(&self) -> bool {
        matches!(self, Self::Everywhere)
    }

    /// Whether the search answers a query that is not a name — a sum, a unit conversion,
    /// an address — as one over a whole workspace does: a search of one app or of the
    /// apps a group can show never does.
    #[must_use]
    pub fn answers(&self) -> bool {
        matches!(self, Self::Workspace | Self::Everywhere)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_widest_search_is_the_one_that_answers_what_is_not_a_name() {
        assert_eq!(Scope::default(), Scope::Workspace);
        assert!(Scope::Workspace.answers());
        assert!(Scope::Everywhere.answers());
        assert!(Scope::Everywhere.everywhere());
        assert!(!Scope::Workspace.everywhere());
        assert!(!Scope::InApp(AppScope::front()).answers());
        assert!(!Scope::WindowGroup(Some(3)).answers());
    }

    #[test]
    fn an_app_scope_names_the_app_and_the_part_of_it() {
        let part = ScopeDecl::new("folder:inbox", "Inbox");
        let scope = AppScope::of("so.tilo.mail").within(part.clone());
        assert_eq!(scope.app.as_deref(), Some("so.tilo.mail"));
        assert_eq!(scope.part, Some(part));
        assert_eq!(AppScope::front(), AppScope::default());
        assert_eq!(
            AppScope::front().named("so.tilo.notes").app.as_deref(),
            Some("so.tilo.notes")
        );
    }

    #[test]
    fn a_window_group_scope_names_the_group_a_hit_opens_in() {
        assert_eq!(Scope::WindowGroup(Some(7)).window_group(), Some(7));
        assert_eq!(Scope::Workspace.window_group(), None);
    }
}
