//! A text field's context menu: what a right-click offers, the action behind each
//! row, and the labels a test reads.

use vello::kurbo::Point;

use super::TextActionRequest;
use crate::text_selection::{clipboard_paste, word_boundaries};
use crate::widget::context_menu::{ContextMenu, MenuItem};

/// `MenuItem` callbacks are `Arc<dyn Fn>` and can't mutate the widget, so
/// clicks dispatch through this parallel list by `item_at` index instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CtxAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
    ApplySpelling {
        start: usize,
        end: usize,
        original: String,
        replacement: String,
    },
    AddToDictionary(String),
    IgnoreInField(String),
    UndoAutocorrection,
    TextAction(usize),
}

impl super::TextInput {
    /// If nothing is selected, the word under the click is selected first
    /// (matches every native editor's right-click-on-a-word behaviour).
    pub(super) fn open_context_menu(&mut self, pos: [f32; 2]) {
        self.activate_spelling();
        if !self.has_selection() {
            let byte = self.resolve_cursor(Point::new(f64::from(pos[0]), f64::from(pos[1])));
            let (start, end) = word_boundaries(&self.text, byte);
            if start == end {
                self.core.set_caret(byte.min(self.text.len()));
            } else {
                self.core.set_selection(start, end);
            }
        }

        let can_share = self.shareable_selection().is_some();
        let can_paste = !self.read_only && clipboard_paste().is_some_and(|s| !s.is_empty());

        let mut menu = ContextMenu::new();
        let mut actions: Vec<Option<CtxAction>> = Vec::new();
        self.refresh_spelling();
        if let (Some((start, end)), Some(correction)) =
            (self.selection_range(), self.last_autocorrection.as_ref())
            && !self.read_only
            && start == correction.start
            && end == correction.end
        {
            menu.push(MenuItem::action(
                format!("Undo autocorrection to ‘{}’", correction.original),
                || {},
            ));
            actions.push(Some(CtxAction::UndoAutocorrection));
            menu.push(MenuItem::separator());
            actions.push(None);
        }
        if let Some((start, end)) = self.selection_range()
            && !self.read_only
            && self.spell_spans.contains(&(start, end))
            && let Some(word) = self.text.get(start..end)
        {
            let word = word.to_string();
            for replacement in spellcheck::suggestions_for_language(&self.spell_language, &word, 3)
            {
                menu.push(MenuItem::action(replacement.clone(), || {}));
                actions.push(Some(CtxAction::ApplySpelling {
                    start,
                    end,
                    original: word.clone(),
                    replacement,
                }));
            }
            if self
                .personal_spelling
                .as_ref()
                .is_some_and(|personal| personal.snapshot.is_some())
            {
                menu.push(MenuItem::action(
                    format!("Add ‘{word}’ to dictionary"),
                    || {},
                ));
                actions.push(Some(CtxAction::AddToDictionary(word.clone())));
            }
            menu.push(MenuItem::action("Ignore in this field", || {}));
            actions.push(Some(CtxAction::IgnoreInField(word)));
            menu.push(MenuItem::separator());
            actions.push(None);
        }
        if can_share && !self.text_actions.is_empty() {
            for (index, action) in self.text_actions.iter().enumerate() {
                menu.push(MenuItem::action(action.label.clone(), || {}));
                actions.push(Some(CtxAction::TextAction(index)));
            }
            menu.push(MenuItem::separator());
            actions.push(None);
        }
        if can_share {
            if !self.read_only {
                menu.push(MenuItem::action("Cut", || {}));
                actions.push(Some(CtxAction::Cut));
            }
            menu.push(MenuItem::action("Copy", || {}));
            actions.push(Some(CtxAction::Copy));
        }
        if can_paste {
            menu.push(MenuItem::action("Paste", || {}));
            actions.push(Some(CtxAction::Paste));
        }
        if !menu.items().is_empty() {
            menu.push(MenuItem::separator());
            actions.push(None);
        }
        menu.push(MenuItem::action("Select All", || {}));
        actions.push(Some(CtxAction::SelectAll));
        menu.set_about(
            self.shareable_selection().map(|text| {
                crate::shared_selection::SharedSelection::of_text(text).within(self.rect)
            }),
        );
        menu.open_at(pos);
        *self.context_menu = menu;
        self.context_actions = actions;
    }

    /// Open the native text menu at a logical-pixel position. Hosts use this
    /// for keyboard menu keys, accessibility actions, and deterministic UI
    /// previews; pointer right-clicks route here automatically.
    pub fn open_text_menu_at(&mut self, pos: [f32; 2]) {
        self.repaint = true;
        self.open_context_menu(pos);
    }

    /// Out-of-range / separator rows are ignored.
    pub(super) fn dispatch_context_action(&mut self, idx: usize) -> bool {
        let Some(Some(action)) = self.context_actions.get(idx).cloned() else {
            return false;
        };
        if self.read_only
            && matches!(
                action,
                CtxAction::Cut
                    | CtxAction::Paste
                    | CtxAction::ApplySpelling { .. }
                    | CtxAction::UndoAutocorrection
            )
        {
            return false;
        }
        match action {
            CtxAction::Cut => self.do_cut(),
            CtxAction::Copy => self.do_copy(),
            CtxAction::Paste => self.do_paste(),
            CtxAction::SelectAll => self.do_select_all(),
            CtxAction::ApplySpelling {
                start,
                end,
                original,
                replacement,
            } => {
                if self
                    .replace_step(start..end, &original, &replacement)
                    .is_none()
                {
                    return false;
                }
                self.spell_cache_key = None;
            }
            CtxAction::AddToDictionary(word) => {
                let result = crate::personal_dictionary::snapshot().and_then(|snapshot| {
                    crate::personal_dictionary::edit(
                        snapshot.revision,
                        crate::personal_dictionary::Edit::Add(
                            crate::personal_dictionary::Word::new(&word, None)?,
                        ),
                    )
                });
                if let Err(error) = result {
                    tracing::warn!(%error, "could not add the shared personal word");
                }
                self.spell_cache_key = None;
                self.refresh_spelling();
            }
            CtxAction::IgnoreInField(word) => {
                self.ignored_spellings.insert(word.to_lowercase());
                self.spell_cache_key = None;
                self.refresh_spelling();
            }
            CtxAction::UndoAutocorrection => {
                let _ = self.undo_last_autocorrection(false);
            }
            CtxAction::TextAction(index) => {
                if let (Some(action), Some((start, end)), Some(selected)) = (
                    self.text_actions.get(index),
                    self.selection_range(),
                    self.shareable_selection(),
                ) {
                    self.pending_text_action = Some(TextActionRequest {
                        id: action.id.clone(),
                        text: selected.to_string(),
                        range: (start, end),
                    });
                }
            }
        }
        self.context_menu.close();
        true
    }

    #[cfg(test)]
    pub(super) fn context_menu_labels(&self) -> Vec<String> {
        if !self.context_menu.is_open() {
            return Vec::new();
        }
        self.context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect()
    }
}
