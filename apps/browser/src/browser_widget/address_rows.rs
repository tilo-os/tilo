//! The list under the address: the omnibox's rows for what is typed
//! (pages visited before, the engine's searches, an inline completion), or,
//! after an extension's keyword and a space, the extension's own, and what
//! choosing one does.

use ui::widget::Widget;

use super::{BrowserWidget, URL_INPUT_ID, row_destination};
use crate::omnibox;

impl BrowserWidget {
    /// Forget everything the omnibox derived from the text being typed.
    pub(super) fn reset_omnibox(&mut self) {
        self.suggestions.clear();
        self.bar_rows.clear();
        self.suggest_debounce.cancel();
        self.remote.clear();
        self.last_query = None;
        self.last_typed.clear();
        self.inline = None;
        self.select_completion = None;
    }

    /// Refresh the address-bar suggestions while the address input is being
    /// edited; hide them otherwise. Runs every frame, does bounded work, and
    /// never waits on the network: the engine's suggestions arrive later
    /// through `suggest_fetcher`.
    pub(super) fn update_suggestions(&mut self) {
        if self.force_suggestions {
            return; // headless overlay render — keep the pinned list
        }
        let text = if ui::window_toolbar::is_hosted() {
            // The address is edited in the window's bar: what the bar said
            // is being typed there.
            self.bar_query.clone().unwrap_or_default()
        } else if self.show_settings || !self.url_input_focused() {
            String::new()
        } else {
            self.address.with(Clone::clone)
        };
        // The bar echoing the completion we put there is not new typing.
        let typed = match &self.inline {
            Some((typed, full)) if *full == text => typed.clone(),
            _ => {
                self.inline = None;
                text
            }
        };
        // After an extension's keyword and a space, what follows is the
        // extension's: it hears of each change, and its rows stand for the
        // omnibox's.
        if typed != self.keyword_typed {
            typed.clone_into(&mut self.keyword_typed);
            let addressed = self.extensions.omnibox_typed(&typed, &self.engine);
            self.keyword_dirty |= addressed == Some(true);
            if self.keyword_active && addressed.is_none() {
                // Back to the omnibox's own rows.
                self.last_query = None;
            }
            self.keyword_active = addressed.is_some();
        }
        if self.keyword_active {
            if std::mem::take(&mut self.keyword_dirty) {
                self.show_keyword_rows(typed.trim());
            }
            return;
        }
        let query = typed.trim();
        // Only suggest while the user is editing a *query* — not when the
        // bar merely shows the loaded page's URL, and not for an empty bar.
        let editing = !query.is_empty() && query != self.current_uri.trim();
        if !editing {
            if self.last_query.is_some()
                || self.suggestions.is_visible()
                || !self.bar_rows.is_empty()
            {
                self.reset_omnibox();
            }
            return;
        }
        let query = query.to_owned();
        let mut dirty = self.last_query.as_deref() != Some(query.as_str());
        if dirty {
            self.on_query_changed(&query);
        }
        if let Some(answer) = self.suggest_fetcher.take()
            && self.suggest_debounce.is_current(answer.generation)
            && answer.query == query
        {
            self.remote = answer.suggestions;
            dirty = true;
        }
        if let Some((generation, wanted)) = self
            .suggest_debounce
            .due(ui::time::now(), omnibox::DEBOUNCE)
            && let Some(endpoint) = omnibox::suggest_endpoint(&self.settings.search_engine, &wanted)
        {
            self.suggest_fetcher.fetch(generation, wanted, endpoint);
        }
        if dirty || self.omni_stale {
            self.rebuild_rows(&query);
        }
    }

    /// The typed text changed: restart the quiet period for the engine's
    /// suggestions and keep only the old ones that still fit.
    pub(super) fn on_query_changed(&mut self, query: &str) {
        let lower = query.to_lowercase();
        self.remote
            .retain(|suggestion| suggestion.to_lowercase().starts_with(&lower));
        if query.chars().count() >= 2 && !omnibox::looks_like_address(query) {
            self.suggest_debounce.typed(query, ui::time::now());
        } else {
            self.suggest_debounce.cancel();
            self.remote.clear();
        }
    }

    /// Select the untyped tail of an inline completion, once the address
    /// field holds the completed text.
    pub(super) fn select_inline_completion(&mut self) {
        fn select(widget: &mut dyn Widget, state: &str) -> bool {
            if widget.test_id() == Some(URL_INPUT_ID) {
                ui::undo::set_state(widget, state);
                return true;
            }
            widget
                .children_mut()
                .into_iter()
                .any(|child| select(child, state))
        }
        let Some((end, start)) = self.select_completion.take() else {
            return;
        };
        if let Some(c) = self.composed.element_mut() {
            select(c, &format!("{end}:b:{start}"));
        }
    }

    /// Match, merge and complete for `query`; set the dropdown's rows.
    pub(super) fn rebuild_rows(&mut self, query: &str) {
        if self.omni_stale {
            self.omni_index.rebuild(self.history.entries());
            self.omni_stale = false;
        }
        let now = tilo_clock::unix_now();
        let matches = self.omni_index.search(query, omnibox::MAX_ROWS, now);
        let mut rows = omnibox::build_rows(
            query,
            &matches,
            &self.remote,
            &self.current_uri,
            omnibox::MAX_ROWS,
        );
        // Complete inline only when the person is adding characters; after a
        // deletion the text is theirs.
        let growing = query.len() > self.last_typed.len() && query.starts_with(&self.last_typed);
        if growing
            && self.inline.is_none()
            && let Some(full) = omnibox::inline_completion(query, matches.first())
        {
            self.select_completion = Some((full.len(), query.len()));
            self.address.set(full.clone());
            self.inline = Some((query.to_owned(), full));
        }
        if let Some((typed, full)) = &self.inline
            && typed == query
        {
            omnibox::lead_with_address(&mut rows, full);
        }
        query.clone_into(&mut self.last_typed);
        self.last_query = Some(query.to_owned());
        self.show_rows(rows);
    }

    /// The rows of the extension typed to after its keyword: what Enter
    /// sends it, then what it suggests. Each goes to the extension with its
    /// keyword, whichever list shows it.
    pub(super) fn show_keyword_rows(&mut self, query: &str) {
        let rows: Vec<omnibox::Row> = self
            .extensions
            .omnibox_rows()
            .map(|(keyword, rows)| {
                rows.into_iter()
                    .take(omnibox::MAX_ROWS)
                    .map(|row| omnibox::Row {
                        kind: omnibox::RowKind::Extension,
                        title: row.title,
                        detail: keyword.name.clone(),
                        url: format!("{} {}", keyword.keyword, row.content),
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.inline = None;
        query.clone_into(&mut self.last_typed);
        self.last_query = Some(query.to_owned());
        self.show_rows(rows);
    }

    /// Show `rows` in the list under the address: the window's bar's while
    /// it hosts the address, else the window's own.
    pub(super) fn show_rows(&mut self, rows: Vec<omnibox::Row>) {
        if ui::window_toolbar::is_hosted() {
            // The bar lists them under its address; the window lists none.
            self.suggestions.clear();
            self.bar_rows = rows;
        } else {
            self.bar_rows.clear();
            self.suggestions
                .set_search_engine(&crate::settings::search_engine_name(
                    &self.settings.search_engine,
                ));
            self.suggestions.set_suggestions(rows);
        }
    }

    /// Open a chosen dropdown row.
    pub(super) fn choose_row(&mut self, row: &omnibox::Row) {
        self.suggestions.clear();
        self.last_query = None;
        self.inline = None;
        if row.kind == omnibox::RowKind::Extension {
            // Enter on the extension's suggestion: its keyword and content.
            self.address.set(row.url.clone());
            self.navigate_to_input();
            return;
        }
        let destination = row_destination(row, &self.settings.search_engine);
        self.address.set(destination.clone());
        self.queue_navigate(destination);
    }
}
