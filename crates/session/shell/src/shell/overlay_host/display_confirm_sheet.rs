//! The question about a tool's display change
//! ([`tilo_sheets::display_confirm`]) as the shell hosts it: the shell keeps
//! the transaction and its countdown and tells the app what to ask on every
//! display; the person's answer comes back and is recorded for the backend.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

use tilo_sheets::display_confirm::{ConfirmEffect, ConfirmSeat, Question};

use super::sheet_apps::{SheetHost, mount_sheet};

impl Default for SheetHost<ConfirmSeat> {
    fn default() -> Self {
        Self::new(
            LayerId::DisplayConfirmation,
            LayerId::DisplayConfirmation.surface_kind(),
            tilo_sheets::display_confirm::definition,
        )
    }
}

impl ShellOverlayHost {
    /// Ask the person to keep or revert display transaction `transaction`,
    /// which a layout tool applied. Settings and the Super+P sheet ask in
    /// their own UI; this is for changes from anywhere else.
    pub fn show_display_confirmation(
        &mut self,
        transaction: u64,
        tool: Option<settings_store::display_tools::DisplayTool>,
    ) {
        self.layers
            .display_question
            .show(transaction, tool, Instant::now());
        let room = self.sheet_room();
        let apps = self.sheet_apps();
        self.layers
            .display_confirm
            .mount(&room, |definition, room, instance, seat| {
                mount_sheet(apps.as_ref(), definition, room, instance, seat)
            });
        self.tell_display_question();
        self.layers.display_confirm.open();
        self.bump_chrome();
    }

    /// Whether the keep-or-revert question for a tool's display change is
    /// up. It owns the keyboard and pointer while it is.
    pub fn display_confirmation_visible(&self) -> bool {
        self.layers.display_question.visible()
    }

    /// Re-read the transaction the question asks about now: close it if it
    /// was answered elsewhere, revert it if its time ran out. Returns
    /// whether the question closed.
    pub fn poll_display_confirmation(&mut self) -> bool {
        self.record_display_answers();
        let closed = self.layers.display_question.poll(Instant::now(), true);
        self.tell_display_question();
        if closed {
            self.bump_chrome();
        }
        closed
    }

    /// The displays the question stands on, one card on each.
    pub fn display_confirmation_displays(&self) -> Vec<Rect> {
        let origin = self.layers.display_confirm.area().origin().to_vec2();
        self.layers
            .display_confirm
            .seat()
            .question()
            .displays
            .into_iter()
            .map(|display| vello::kurbo::Affine::translate(origin).transform_rect_bbox(display))
            .collect()
    }

    /// The question as assistive technology reads it, once its app drew it:
    /// the dialog, with what a reader meets in it after it, in order.
    pub fn display_confirmation_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut order =
            ui::flatten_reading_order(self.layers.display_confirm.widget()?).into_iter();
        let mut dialog = order
            .by_ref()
            .find(|node| node.role == ui::a11y::Role::Dialog)?;
        dialog.children = order.filter(|node| !node.state.hidden).collect();
        Some(dialog)
    }

    /// Record what the person answered, follow the transaction, and keep
    /// the question current; whether anything changed on screen.
    pub(super) fn pump_display_confirmation(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        let shown = self.layers.display_confirm.update(doc, path);
        let answered = self.record_display_answers();
        let closed = self.layers.display_question.poll(Instant::now(), false);
        self.tell_display_question();
        shown | answered | closed
    }

    fn record_display_answers(&mut self) -> bool {
        let answers = self.layers.display_confirm.seat().take_asked();
        let answered = !answers.is_empty();
        for answer in answers {
            match answer {
                ConfirmEffect::Keep => self.layers.display_question.keep(),
                ConfirmEffect::Revert => self.layers.display_question.revert(),
                ConfirmEffect::KeepAndTrust => self.layers.display_question.always_keep(),
            }
        }
        answered
    }

    /// Tell the app what to ask now, or take the question down once the
    /// transaction no longer waits.
    fn tell_display_question(&self) {
        let question = &self.layers.display_question;
        if !question.visible() {
            self.layers.display_confirm.close_now();
            return;
        }
        let to_sheet = vello::kurbo::Affine::translate(self.rect.origin().to_vec2()).inverse();
        let displays = self
            .layer_outputs()
            .into_iter()
            .filter(|display| display.width() > 0.0 && display.height() > 0.0)
            .map(|display| to_sheet.transform_rect_bbox(display))
            .collect();
        self.layers.display_confirm.seat().ask(Question {
            tool: question.tool().map(|tool| tool.name().to_owned()),
            status: question.status(),
            answering: question.answering(),
            displays,
        });
    }
}
