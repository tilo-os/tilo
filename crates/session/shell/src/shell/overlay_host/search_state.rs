//! What search is showing, as the shell keeps it for the launcher: the
//! query, the rows and the one selected, the argument step on screen and
//! how the field reads. Dispatch (opening search, running a row, collecting
//! an action's arguments) changes it; the launcher is shown what it holds
//! ([`super::launcher_host`]) and draws it. Nothing here is drawn or laid
//! out.

use tilo_search::FieldWeight;
use ui::{ArgumentSpec, CommandEntry};

/// Rows a full list holds until the launcher says how many fit its card.
const DEFAULT_ROW_CAPACITY: usize = 10;

/// Search as it stands.
#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one independent part of what search shows"
)]
pub(crate) struct SearchState {
    /// What was typed for the results. While an argument step holds the
    /// field it waits here, and comes back when the step ends.
    query: String,
    /// The argument step on screen: its id and what it asks.
    step: Option<(String, ArgumentSpec)>,
    entries: Vec<CommandEntry>,
    /// The entries shown, by index: every one, or those the query matches
    /// while typing narrows the list.
    visible: Vec<usize>,
    cursor: usize,
    /// Typing narrows the rows here (a step's values, a row's other
    /// actions) rather than asking the search registry.
    narrows: bool,
    index: tilo_search::ListIndex,
    placeholder: String,
    scope_token: Option<String>,
    scope_token_action: Option<String>,
    field_only_while_empty: bool,
    band: bool,
    /// How many rows the launcher's list holds, once it said.
    row_capacity: Option<usize>,
    /// Why a row did not run: its id, a title and a detail. It stays on
    /// that row while the same query's rows are rebuilt (a notice the run
    /// posted moves the chrome), and goes when the query changes.
    status: Option<(String, String, String)>,
}

impl SearchState {
    /// Search closes; its rows stay for the next time it opens.
    pub(crate) fn close(&mut self) {
        self.step = None;
        self.status = None;
    }

    /// Replace the results' query.
    pub(crate) fn set_query(&mut self, query: &str) {
        if self.query != query {
            self.status = None;
        }
        query.clone_into(&mut self.query);
        self.rebuild();
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// Replace the rows; the first is selected.
    pub(crate) fn set_entries(&mut self, entries: Vec<CommandEntry>) {
        self.entries = entries;
        self.cursor = 0;
        if let Some((id, title, detail)) = &self.status
            && let Some(entry) = self.entries.iter_mut().find(|entry| &entry.id == id)
        {
            entry.badge = Some(title.clone());
            entry.description = Some(detail.clone());
        }
        self.rebuild();
    }

    /// Every row, before any narrowing.
    pub(crate) fn entries(&self) -> &[CommandEntry] {
        &self.entries
    }

    /// The rows shown, in order.
    pub(crate) fn visible_entries(&self) -> Vec<&CommandEntry> {
        self.visible
            .iter()
            .filter_map(|&at| self.entries.get(at))
            .collect()
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Select shown row `at`, or the last one past the end.
    pub(crate) fn set_cursor(&mut self, at: usize) {
        self.cursor = at.min(self.visible.len().saturating_sub(1));
    }

    /// Select the row `id`, if it is shown. Whether it is.
    pub(crate) fn select(&mut self, id: &str) -> bool {
        let Some(at) = self
            .visible_entries()
            .iter()
            .position(|entry| entry.id == id)
        else {
            return false;
        };
        self.cursor = at;
        true
    }

    /// The selected row's id; `None` while nothing is shown.
    pub(crate) fn selected_id(&self) -> Option<&str> {
        self.visible
            .get(self.cursor)
            .and_then(|&at| self.entries.get(at))
            .map(|entry| entry.id.as_str())
    }

    /// Say on the selected row why it did not run.
    pub(crate) fn set_selected_status(
        &mut self,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) {
        let Some(&at) = self.visible.get(self.cursor) else {
            return;
        };
        if let Some(entry) = self.entries.get_mut(at) {
            let (title, detail) = (title.into(), detail.into());
            entry.badge = Some(title.clone());
            entry.description = Some(detail.clone());
            self.status = Some((entry.id.clone(), title, detail));
        }
    }

    /// Why the selected row did not run: a title and a detail.
    pub(crate) fn selected_status(&self) -> Option<(&str, &str)> {
        let (id, title, detail) = self.status.as_ref()?;
        (self.selected_id() == Some(id.as_str())).then_some((title.as_str(), detail.as_str()))
    }

    /// Whether typing narrows the rows here (`false`) or the rows show as
    /// the registry ranked them (`true`).
    pub(crate) fn set_bypass_fuzzy(&mut self, bypass: bool) {
        self.narrows = !bypass;
        self.rebuild();
    }

    pub(crate) fn bypasses_fuzzy(&self) -> bool {
        !self.narrows
    }

    pub(crate) fn set_query_placeholder(&mut self, placeholder: impl Into<String>) {
        self.placeholder = placeholder.into();
    }

    pub(crate) fn query_placeholder(&self) -> &str {
        &self.placeholder
    }

    pub(crate) fn set_scope_token(&mut self, token: Option<String>) {
        self.scope_token = token;
    }

    pub(crate) fn scope_token(&self) -> Option<&str> {
        self.scope_token.as_deref()
    }

    pub(crate) fn set_scope_token_action(&mut self, action: Option<String>) {
        self.scope_token_action = action;
    }

    pub(crate) fn scope_token_action(&self) -> Option<&str> {
        self.scope_token_action.as_deref()
    }

    /// The field stands alone while nothing is typed.
    pub(crate) fn set_field_only_while_empty(&mut self, on: bool) {
        self.field_only_while_empty = on;
    }

    pub(crate) fn field_only_while_empty(&self) -> bool {
        self.field_only_while_empty
    }

    #[cfg(test)]
    /// Whether the field shows alone now: empty and asked to stand alone,
    /// or a step with nothing listed under it.
    pub(crate) fn shows_field_only(&self) -> bool {
        (self.field_only_while_empty && self.query.is_empty() && self.step.is_none())
            || (self.step.is_some() && self.visible.is_empty())
    }

    /// Show or hide the filter band. Whether that changed.
    pub(crate) fn set_filter_band_shown(&mut self, shown: bool) -> bool {
        std::mem::replace(&mut self.band, shown) != shown
    }

    pub(crate) fn filter_band_shown(&self) -> bool {
        self.band
    }

    /// Ask for one value in the field (`id`, as `spec` asks it), the
    /// results' query kept until the step ends.
    pub(crate) fn open_argument_step(&mut self, id: impl Into<String>, spec: ArgumentSpec) {
        self.step = Some((id.into(), spec));
    }

    /// Leave the argument step on screen, if any.
    pub(crate) fn cancel_argument(&mut self) {
        self.step = None;
    }

    #[cfg(test)]
    pub(crate) fn in_argument_mode(&self) -> bool {
        self.step.is_some()
    }

    /// The argument step on screen: its id and what it asks.
    pub(crate) fn argument_step(&self) -> Option<(&str, &ArgumentSpec)> {
        self.step.as_ref().map(|(id, spec)| (id.as_str(), spec))
    }

    /// How many rows the launcher's list holds.
    pub(crate) fn entry_row_capacity(&self) -> usize {
        self.row_capacity.unwrap_or(DEFAULT_ROW_CAPACITY).max(1)
    }

    /// The launcher's list holds `rows`. Whether that changed.
    pub(crate) fn set_row_capacity(&mut self, rows: usize) -> bool {
        self.row_capacity.replace(rows) != Some(rows)
    }

    fn rebuild(&mut self) {
        self.visible.clear();
        let ranks = self.narrows.then(|| {
            self.index.ranks(
                &self.query,
                self.entries.iter().map(|entry| {
                    [
                        (entry.label.as_str(), FieldWeight::TITLE),
                        (
                            entry.description.as_deref().unwrap_or_default(),
                            FieldWeight::SUBTITLE,
                        ),
                    ]
                }),
            )
        });
        for at in 0..self.entries.len() {
            if ranks
                .as_ref()
                .is_some_and(|ranks| ranks.get(at).is_none_or(Option::is_none))
            {
                continue;
            }
            self.visible.push(at);
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(state: &SearchState) -> Vec<&str> {
        state
            .visible_entries()
            .into_iter()
            .map(|entry| entry.id.as_str())
            .collect()
    }

    #[test]
    fn typing_narrows_only_a_list_that_narrows_here() {
        let mut state = SearchState::default();
        state.set_entries(vec![
            CommandEntry::new("a", "Alpha"),
            CommandEntry::new("b", "Beta"),
        ]);
        state.set_query("bet");
        assert_eq!(
            rows(&state),
            ["a", "b"],
            "the registry's rows show as ranked"
        );
        state.set_bypass_fuzzy(false);
        assert_eq!(rows(&state), ["b"], "a step's values narrow as typed");
        assert_eq!(state.selected_id(), Some("b"));
    }

    #[test]
    fn a_step_keeps_the_query_and_hands_it_back() {
        let mut state = SearchState::default();
        state.set_query("scale");
        state.open_argument_step("param:value", ArgumentSpec::default());
        assert!(state.in_argument_mode());
        assert!(state.shows_field_only(), "nothing listed under the step");
        state.cancel_argument();
        assert_eq!(state.query(), "scale");
        assert!(!state.shows_field_only());
    }
}
