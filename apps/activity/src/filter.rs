//! The app's record of the workspace: which changes History shows.

use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;

/// Which changes History shows.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Period
    pub period: String,
    /// Person
    pub author: String,
    /// Browse by
    pub lens: String,
    /// Search
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

    /// Whether the window bar's search is on: the feed keeps reading past one page while it
    /// is.
    pub fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Whether `haystack`'s words contain the search's query, ignoring case.
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

impl Filter {
    /// What the record holds: which changes History shows — the agent, the workspace and
    /// the span of time, each picked whole from a list.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "activity.filter",
    read_content = Filter::read_document
)]
impl Filter {
    /// Show every change again.
    #[action(label = "activity-clear-filters")]
    pub fn clear_filters(&mut self) {
        *self = Self::default();
    }
}
