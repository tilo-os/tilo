//! The mention picker a rich text field opens at an `@`: the members it offers, following the caret,
//! and what a key does to it. What a query is and how members rank is the toolkit's
//! (`ui::widget::mention_picker`), which the plain text completion shares.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use ui::widget::autocomplete::SuggestionList;
use ui::widget::mention_picker::{MentionCandidate, query_at_caret, rank, suggestion};

/// What a key did to an open picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerKey {
    Pass,
    Handled,
    Pick(MentionCandidate),
}

/// The open picker of one field: which `@` it answers and the rows shown.
#[derive(Default)]
pub(crate) struct MentionPicker {
    list: SuggestionList,
    shown: Vec<MentionCandidate>,
    open: Option<(usize, usize)>,
    typed: usize,
    dismissed: Option<(usize, usize)>,
}

impl MentionPicker {
    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some() && !self.shown.is_empty()
    }

    /// The block and byte of the `@` the open list answers.
    pub(crate) fn anchor(&self) -> Option<(usize, usize)> {
        self.open.filter(|_| self.is_open())
    }

    /// Bytes before the caret a pick replaces: the `@` and what followed.
    pub(crate) fn typed(&self) -> usize {
        self.typed
    }

    pub(crate) fn close(&mut self) {
        self.open = None;
        self.shown.clear();
        self.list.clear();
    }

    /// Follow the caret: `before` is the focused block's text up to the
    /// caret, `in_mention` whether the `@` there is already a mention's.
    /// Opens, narrows or closes the list; returns whether anything shown
    /// changed.
    pub(crate) fn follow(
        &mut self,
        block: usize,
        before: &str,
        in_mention: impl Fn(usize) -> bool,
        candidates: &[MentionCandidate],
    ) -> bool {
        let was = (self.open, self.shown.clone());
        let query = query_at_caret(before, before.len()).filter(|query| !in_mention(query.start));
        match query {
            Some(query) if self.dismissed != Some((block, query.start)) => {
                self.dismissed = None;
                let ranked: Vec<MentionCandidate> = rank(candidates, &query.query)
                    .into_iter()
                    .cloned()
                    .collect();
                if ranked.is_empty() {
                    self.close();
                } else {
                    let keep = self
                        .list
                        .highlighted()
                        .and_then(|row| ranked.iter().position(|c| c.reference() == row.value));
                    self.list.set_rows(ranked.iter().map(suggestion).collect());
                    self.list.set_highlight(Some(keep.unwrap_or(0)));
                    self.shown = ranked;
                    self.open = Some((block, query.start));
                    self.typed = before.len().saturating_sub(query.start);
                }
            }
            Some(_) => self.close(),
            None => {
                self.dismissed = None;
                self.close();
            }
        }
        was != (self.open, self.shown.clone())
    }

    /// A key while the list is open.
    pub(crate) fn handle_key(&mut self, event: &WindowEvent) -> PickerKey {
        if !self.is_open() {
            return PickerKey::Pass;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(key),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return PickerKey::Pass;
        };
        match key {
            NamedKey::ArrowDown | NamedKey::ArrowUp => {
                self.list.move_highlight(*key == NamedKey::ArrowDown);
                PickerKey::Handled
            }
            NamedKey::Enter | NamedKey::Tab => self.picked(self.list.highlight()),
            NamedKey::Escape => {
                self.dismissed = self.open;
                self.close();
                PickerKey::Handled
            }
            _ => PickerKey::Pass,
        }
    }

    /// Whether a key release or repeat of an arrow, Enter, Tab or Escape belongs to the
    /// open list (so the field does not act on it too).
    pub(crate) fn owns_key(&self, event: &WindowEvent) -> bool {
        self.is_open()
            && matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        logical_key: Key::Named(
                            NamedKey::ArrowDown
                                | NamedKey::ArrowUp
                                | NamedKey::Enter
                                | NamedKey::Tab
                                | NamedKey::Escape
                        ),
                        ..
                    },
                    ..
                }
            )
    }

    fn picked(&self, index: Option<usize>) -> PickerKey {
        let Some(candidate) = index.and_then(|index| self.shown.get(index)).cloned() else {
            return PickerKey::Handled;
        };
        PickerKey::Pick(candidate)
    }

    /// The row under `point`, picked.
    pub(crate) fn click_at(&self, point: Point) -> Option<MentionCandidate> {
        if !self.is_open() {
            return None;
        }
        match self.picked(self.list.row_at(point)) {
            PickerKey::Pick(candidate) => Some(candidate),
            _ => None,
        }
    }

    /// Whether `point` is over the open list.
    pub(crate) fn contains(&self, point: Point) -> bool {
        self.is_open() && self.list.rect().is_some_and(|rect| rect.contains(point))
    }

    /// Arm the row under `point`; returns whether that changed anything.
    pub(crate) fn hover_at(&mut self, point: Point) -> bool {
        if !self.is_open() {
            return false;
        }
        let changed = self.list.hover_at(point);
        if let Some(row) = self.list.row_at(point)
            && self.list.highlight() != Some(row)
        {
            self.list.set_highlight(Some(row));
            return true;
        }
        changed
    }

    /// The list's panel in the last paint.
    pub(crate) fn rect(&self) -> Option<Rect> {
        self.is_open().then(|| self.list.rect()).flatten()
    }

    /// Paint the list under `caret` (the `@`'s caret rect), inside `viewport` when the
    /// window's is known.
    pub(crate) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        caret: Rect,
        viewport: Option<Rect>,
    ) {
        if !self.is_open() {
            return;
        }
        self.list.set_anchor(caret);
        if let Some(viewport) = viewport {
            self.list.set_viewport(viewport);
        }
        self.list.paint(scene, transform, text, theme);
    }

    /// The rows in view, for assistive technology.
    pub(crate) fn access_rows(&self) -> Vec<tilo_ui_base::a11y::AccessNode> {
        if !self.is_open() {
            return Vec::new();
        }
        self.list.access_rows(|row| {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            "mention".hash(&mut hasher);
            row.value.hash(&mut hasher);
            hasher.finish()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::MemberId;
    use tilo_ui_base::tokens::PresenceHue;

    fn workspace() -> Vec<MentionCandidate> {
        vec![
            MentionCandidate::new(
                MemberId::person("u_ada"),
                "Ada Lovelace",
                PresenceHue::of_identity("u_ada"),
            ),
            MentionCandidate::new(
                MemberId::person("u_alan"),
                "Alan Turing",
                PresenceHue::of_identity("u_alan"),
            ),
            MentionCandidate::new(
                MemberId::agent("tide", "u_alan"),
                "Tide",
                PresenceHue::of_identity("agent:tide"),
            ),
        ]
    }

    tilo_test_env::scenario! {
        fn the_mention_picker_follows_the_caret;

        phase the_picker_follows_the_caret_and_stays_closed_after_escape;
        {
            let _settled = tilo_ui_base::motion::testing::settled();
            let workspace = workspace();
            let mut picker = MentionPicker::default();
            assert!(picker.follow(0, "hello @a", |_| false, &workspace));
            assert!(picker.is_open());
            assert_eq!(picker.typed(), 2);
            let escape = tilo_ui_base::synthetic_input::key_press(Key::Named(NamedKey::Escape), None);
            assert_eq!(picker.handle_key(&escape), PickerKey::Handled);
            assert!(!picker.is_open());
            picker.follow(0, "hello @al", |_| false, &workspace);
            assert!(!picker.is_open(), "the dismissed @ stays closed");
            picker.follow(0, "hello @al @t", |_| false, &workspace);
            assert!(picker.is_open(), "a new @ opens it again");
            picker.follow(0, "hello @Tide", |at| at == 6, &workspace);
            assert!(!picker.is_open(), "a finished mention offers nothing");
        }
    }
}
