//! An app-local **typeable combobox** for the create-container image field.
//!
//! The `ui` toolkit ships a [`ui::widget::Dropdown`] with an incremental
//! substring *filter*, but that widget commits one of its *fixed options* as
//! the value — you cannot type a brand-new value that isn't in the list. The
//! create-container flow needs the opposite: a free-text field whose typed
//! contents ARE the value (an arbitrary image reference like
//! `ghcr.io/acme/app:edge`), with a convenience dropdown of locally-installed
//! images that filters as you type and can be picked to fill the field.
//!
//! Because the containers app renders with manual draw + hit-testing rather
//! than the retained `Widget` tree, this combobox is a small pure
//! state+logic type: it owns the open/closed flag and computes the filtered
//! option list; `panels.rs` draws it and routes clicks/keys. The *value*
//! itself lives in the shared CRDT (`CreateField::Image`) so it round-trips
//! through undo/redo and collaboration like every other field.
//!
//! ## ui need
//! A reusable *typeable* combobox (free-text value + suggestion dropdown)
//! belongs in `ui` next to `Dropdown`. This module is the app-local stand-in
//! until that lands; the filtering kernel here ([`filter_options`]) is the
//! behaviour such a widget would share.

/// The combobox's transient open/highlight state. The committed text value is
/// NOT stored here — it lives in the CRDT create-form draft.
#[derive(Clone, Debug, Default)]
pub struct ComboState {
    /// Whether the suggestion dropdown is showing.
    pub open: bool,
    /// Highlighted row in the *filtered* option list (for keyboard select).
    pub highlight: usize,
}

impl ComboState {
    pub fn close(&mut self) {
        self.open = false;
        self.highlight = 0;
    }

    /// Clamp the highlight into the current filtered length and move it by
    /// `delta` (saturating). Opens the dropdown if it was closed.
    pub fn move_highlight(&mut self, delta: i32, filtered_len: usize) {
        self.open = true;
        if filtered_len == 0 {
            self.highlight = 0;
            return;
        }
        let max = filtered_len - 1;
        let next = (self.highlight as i32 + delta).clamp(0, max as i32);
        self.highlight = next as usize;
    }
}

/// Case-insensitive substring filter over `options` for the typed `query`,
/// returning indices into `options` in original order. An empty query shows
/// every option. This is the same kernel `ui::Dropdown` uses for its filter,
/// lifted out so it can be unit-tested independently of any rendering.
pub fn filter_options(options: &[String], query: &str) -> Vec<usize> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return (0..options.len()).collect();
    }
    options
        .iter()
        .enumerate()
        .filter(|(_, o)| o.to_ascii_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Vec<String> {
        vec![
            "docker.io/library/nginx:1.27-alpine".into(),
            "docker.io/library/postgres:16".into(),
            "docker.io/library/redis:7-alpine".into(),
            "registry.example.test/api:2.4.1".into(),
        ]
    }

    /// The combobox shows all on an empty query, filters case-insensitively as you type, clamps the highlight and resets on close.
    #[test]
    fn combobox_filters_and_clamps_its_highlight() {
        // --- phase: empty_query_shows_all ---
        {
            let o = opts();
            assert_eq!(filter_options(&o, ""), vec![0, 1, 2, 3]);
            assert_eq!(filter_options(&o, "   "), vec![0, 1, 2, 3]);
        }

        // --- phase: filter_is_case_insensitive_substring ---
        {
            let o = opts();
            // "ALPINE" matches nginx + redis (both alpine), in original order.
            assert_eq!(filter_options(&o, "ALPINE"), vec![0, 2]);
            // partial registry match
            assert_eq!(filter_options(&o, "example.test"), vec![3]);
            // no match → empty
            assert!(filter_options(&o, "mariadb").is_empty());
        }

        // --- phase: filter_narrows_as_you_type ---
        {
            let o = opts();
            // Typing progressively narrows the candidate set.
            let by_n = filter_options(&o, "n"); // nginx (nginx) only? 'n' also in 'registry'... check substrings
            let by_ng = filter_options(&o, "ng"); // only nginx
            assert!(by_ng.len() <= by_n.len(), "more chars never widen the set");
            assert_eq!(by_ng, vec![0], "'ng' uniquely matches nginx");
        }

        // --- phase: highlight_clamps_to_filtered_len ---
        {
            let mut s = ComboState::default();
            s.move_highlight(5, 3); // past the end
            assert_eq!(s.highlight, 2, "clamped to last index");
            assert!(s.open, "navigation opens the dropdown");
            s.move_highlight(-10, 3);
            assert_eq!(s.highlight, 0, "clamped to first index");
            // empty list parks at 0 without panicking.
            s.move_highlight(1, 0);
            assert_eq!(s.highlight, 0);
        }

        // --- phase: close_resets ---
        {
            let mut s = ComboState {
                open: true,
                highlight: 2,
            };
            s.close();
            assert!(!s.open);
            assert_eq!(s.highlight, 0);
        }
    }
}
