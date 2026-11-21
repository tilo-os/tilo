//! What History narrows to, as the window holds it: never stored or synced.

use ui::tilo_actions::remote::workspace_activity as wire;

/// The window's filter: a span of time, a person, a browse lens, and what the window bar's
/// search says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Filters {
    /// Period
    pub period: String,
    /// Person
    pub author: String,
    /// Browse by
    pub lens: String,
    /// Search
    pub query: String,
}

impl Filters {
    /// The feed this filter asks for, in `scope`.
    pub fn query(&self, scope: wire::Scope) -> wire::FeedQuery {
        wire::FeedQuery {
            scope,
            period: period(&self.period),
            author: Some(self.author.clone()).filter(|author| !author.is_empty()),
            lens: Some(self.lens.clone()).filter(|lens| !lens.is_empty()),
            resources: None,
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
