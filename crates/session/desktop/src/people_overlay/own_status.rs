//! The person's own row at the top of the overlay: their face beside a
//! field saying, in their own words, what they are doing.
//!
//! It shows what they said; a press takes the keyboard to change it.
//! Enter keeps the words (the host publishes them, and they lead what
//! everyone here reads of them), Escape puts back what was there, and the
//! clear button takes their words away, so an automatic description shows
//! again. Like the message box, it holds the keyboard only once pressed.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::keyboard::{Key, ModifiersState};

use ui::TextRole;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::text_selection::Selection;
use ui::theme::Theme;
use ui::time::Instant;
use ui::widget::{ButtonFace, ButtonRole, ControlSize, FieldFace, TextInput, Widget as _};

use crate::i18n::t;
use crate::text_entry::{self, Edit, Mods};

/// What a key did to the field.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Keyed {
    Ignored,
    Consumed,
    /// Escape: back to what was said; the field lets go.
    Cancel,
    /// Enter: say these words (empty clears them).
    Save(String),
}

#[derive(Default)]
pub(super) struct OwnStatus {
    /// What the person said, as the host last gave it.
    said: String,
    /// The words in the field: `said`, or what is being typed.
    text: String,
    selection: Selection,
    focused: bool,
    input: Option<TextInput>,
    pub(super) field: Rect,
    clear: Rect,
}

/// The field's height in `theme`.
pub(super) fn height(theme: &Theme) -> f64 {
    FieldFace::height(ControlSize::Regular, theme)
}

impl OwnStatus {
    /// What the person said, as the host has it now: shown unless they
    /// are changing it.
    pub(super) fn sync(&mut self, said: &str) {
        if self.said != said {
            said.clone_into(&mut self.said);
            if !self.focused {
                said.clone_into(&mut self.text);
                self.selection = Selection::collapsed(self.text.len());
            }
        }
    }

    pub(super) fn is_focused(&self) -> bool {
        self.focused
    }

    pub(super) fn field_contains(&self, point: Point) -> bool {
        self.field.contains(point) && !self.clear.contains(point)
    }

    /// Where the clear button is, while there are words to clear.
    pub(super) fn clear_rect(&self) -> Option<Rect> {
        (self.clear.area() > 0.0).then_some(self.clear)
    }

    /// A press on the field: it takes the keyboard, the caret at the end.
    pub(super) fn focus(&mut self) {
        self.focused = true;
        self.selection = Selection::collapsed(self.text.len());
    }

    /// Let go of the keyboard, putting back what was said.
    pub(super) fn cancel(&mut self) {
        self.focused = false;
        self.text.clone_from(&self.said);
        self.selection = Selection::collapsed(self.text.len());
        if let Some(input) = self.input.as_mut() {
            input.set_focus(false);
        }
    }

    /// The words cleared: the field lets go, empty, until the host says.
    pub(super) fn cleared(&mut self) {
        self.text.clear();
        self.said.clear();
        self.cancel();
    }

    pub(super) fn next_wake_deadline(&self) -> Option<Instant> {
        self.input
            .as_ref()
            .filter(|_| self.focused)
            .and_then(ui::widget::Widget::next_wake_deadline)
    }

    pub(super) fn commit_text(&mut self, text: &str) -> bool {
        if !self.focused {
            return false;
        }
        text_entry::commit(text, &mut self.text, &mut self.selection);
        self.bound();
        true
    }

    fn bound(&mut self) {
        if self.text.chars().count() > ui::MAX_STATUS_CHARS {
            self.text = self.text.chars().take(ui::MAX_STATUS_CHARS).collect();
            self.selection = Selection::collapsed(self.text.len());
        }
    }

    pub(super) fn key(&mut self, key: &Key, mods: ModifiersState) -> Keyed {
        if !self.focused {
            return Keyed::Ignored;
        }
        match text_entry::apply(
            key,
            Mods::from_state(mods),
            &mut self.text,
            &mut self.selection,
        ) {
            Edit::Submit => {
                let words = self.text.trim().to_owned();
                self.said.clone_from(&words);
                self.cancel();
                Keyed::Save(words)
            }
            Edit::Cancel => {
                self.cancel();
                Keyed::Cancel
            }
            Edit::Changed => {
                self.bound();
                Keyed::Consumed
            }
            // The field holds the keyboard: a key it does not use still
            // must not reach the window behind.
            Edit::ConsumedQuiet | Edit::Ignored => Keyed::Consumed,
        }
    }

    /// Paint the field in `field`.
    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        field: Rect,
        cursor: Option<Point>,
    ) {
        self.field = field;
        let control = f64::from(theme.tokens.metrics.bar_control_height);
        let inset = (field.height() - control) / 2.0;
        self.clear = if self.text.is_empty() {
            Rect::ZERO
        } else {
            Rect::new(
                field.x1 - inset - control,
                field.y0 + inset,
                field.x1 - inset,
                field.y1 - inset,
            )
        };
        let placeholder = t("people-overlay-own-status-placeholder");
        let hovered = cursor.is_some_and(|p| field.contains(p));
        let face = FieldFace::new(field, &self.text)
            .placeholder(&placeholder)
            .hovered(hovered)
            .focused(self.focused);
        if self.focused && !self.text.is_empty() {
            face.paint_chrome(scene, transform, theme);
        } else {
            face.paint(scene, transform, text, theme);
        }
        if self.focused {
            let mut text_rect = face.text_rect(theme);
            if self.clear.area() > 0.0 {
                text_rect.x1 = text_rect.x1.min(self.clear.x0 - 4.0);
            }
            let input = self.input.get_or_insert_with(|| {
                TextInput::new(Vec::new())
                    .with_plain_chrome()
                    .with_text_role(TextRole::Body)
            });
            input.set_text(&self.text);
            input.set_selection(self.selection.anchor, self.selection.cursor);
            input.set_focus(true);
            input.apply_layout_external(text_rect);
            let input_theme = theme
                .clone()
                .with_text_scale(theme.text_scale * theme.ui_scale);
            input.draw(
                scene,
                transform,
                text,
                &input_theme,
                &PresenceCtx::default(),
            );
        }
        if self.clear.area() > 0.0 {
            let label = t("people-overlay-own-status-clear");
            ui::widget::Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::icon_only(self.clear, Icon::X, &label)
                    .role(ButtonRole::Plain)
                    .hovered(cursor.is_some_and(|p| self.clear.contains(p))),
            );
        }
    }
}
