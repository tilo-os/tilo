//! The app's record of the workspace: which changes History shows. It
//! follows the person to every device and is shared with the workspace like
//! any record, so an agent asked to "show me Ada's changes" sets it as the
//! person would.
//!
//! The history itself is the workspace's, read through the session
//! ([`ui::tilo_actions::remote::workspace_activity`]); this record holds no
//! part of it.

use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;

/// Which changes History shows. Each part is picked whole from a list, so
/// each is written whole.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Period
    ///
    /// Only changes from this period: `today`, `week` or `month`; empty for
    /// all time.
    pub period: String,
    /// Person
    ///
    /// Only this person's changes, by the name History shows; empty for
    /// everyone.
    pub author: String,
    /// Browse by
    ///
    /// Only the changes under this lens (an app, a resource, a work group,
    /// agent tasks), by its key; empty for everything.
    pub lens: String,
    /// Find
    ///
    /// Only the changes whose words, person, app or outcome contain this
    /// text; empty for everything.
    pub query: String,
}

impl Filter {
    /// The feed this filter asks for, in `scope`.
    pub fn query(&self, scope: wire::Scope) -> wire::FeedQuery {
        wire::FeedQuery {
            scope,
            period: period(&self.period),
            author: Some(self.author.clone()).filter(|author| !author.is_empty()),
            lens: Some(self.lens.clone()).filter(|lens| !lens.is_empty()),
            limit: 0,
        }
    }

    /// Whether it narrows the history at all.
    pub fn narrows(&self) -> bool {
        self != &Self::default()
    }

    /// Whether a text find is on: the feed keeps reading past one page
    /// while it is.
    pub fn finding(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Whether `haystack`'s words contain the find, ignoring case.
    pub fn matches(&self, haystacks: &[&str]) -> bool {
        let query = self.query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        haystacks
            .iter()
            .any(|text| text.to_lowercase().contains(&query))
    }
}

/// The period `key` names; all time for any other.
pub fn period(key: &str) -> wire::Period {
    match key {
        "today" => wire::Period::Today,
        "week" => wire::Period::Week,
        "month" => wire::Period::Month,
        _ => wire::Period::AllTime,
    }
}

#[resource(kind = "activity.filter", label = "Workspace activity")]
impl Filter {
    /// Show every change again.
    #[action]
    pub fn clear_filters(&mut self) {
        *self = Self::default();
    }
}
