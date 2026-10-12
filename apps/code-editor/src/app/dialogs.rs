//! The editor's two questions: "this file changed on disk" and "save changes
//! before closing?". Both are decisions the user makes — silently choosing for
//! them loses somebody's work — and both have three answers, so they are the
//! toolkit's immediate modal sheet ([`ModalSheet`]): it paints, hit-tests and
//! answers, the editor says what is asked.
//!
//! The sheet is painted in the window's top layer ([`Widget::draw_overlay`]),
//! over the whole window, so its scrim covers the sidebar and the tabs too.
//! Enter takes the answer that keeps everyone's work, Escape cancels, and Tab
//! walks the answers.

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::modal_dialog::SheetResponse;
use ui::widget::{ModalAction, ModalSpec};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use super::{CloseChoice, CodeApp, ConflictChoice};

/// Which question is on screen. A conflict outranks a close prompt: it is the
/// reason a save did not land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Question {
    Conflict,
    ClosePrompt,
}

/// What one question says, in the order its answers are indexed: the safe
/// default, the answer that discards something, Cancel.
struct Wording {
    title: String,
    body: &'static str,
    actions: [ModalAction<'static>; 3],
}

/// The index of Cancel in every question's answers.
const CANCEL: usize = 2;

impl CodeApp {
    /// The question waiting for an answer, if any.
    pub(super) fn question(&self) -> Option<Question> {
        if self.conflict.is_some() {
            Some(Question::Conflict)
        } else if self.close_prompt.is_some() {
            Some(Question::ClosePrompt)
        } else {
            None
        }
    }

    fn wording(&self, question: Question) -> Wording {
        match question {
            Question::Conflict => Wording {
                title: format!(
                    "{} changed on disk",
                    self.conflict.as_ref().map_or("", |c| c.name.as_str())
                ),
                body: "Something else wrote this file after you opened it. Overwriting \
                       discards their change; reloading discards yours.",
                actions: [
                    ModalAction::primary("Reload"),
                    ModalAction::alternate_danger("Overwrite"),
                    ModalAction::secondary("Cancel"),
                ],
            },
            Question::ClosePrompt => Wording {
                title: format!(
                    "Save changes to {}?",
                    self.close_prompt.as_ref().map_or("", |p| p.name.as_str())
                ),
                body: "This file has unsaved changes. If you don't save them, they will \
                       be lost.",
                actions: [
                    ModalAction::primary("Save"),
                    ModalAction::alternate_danger("Don't save"),
                    ModalAction::secondary("Cancel"),
                ],
            },
        }
    }

    /// Paint the open question over the whole `window`.
    pub(super) fn draw_question(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        let Some(question) = self.question() else {
            self.question_sheet.reset();
            return;
        };
        let wording = self.wording(question);
        let spec = ModalSpec::new(&wording.title)
            .actions(&wording.actions)
            .closable(false)
            .dismiss_on_scrim(false);
        self.question_sheet.paint_message(
            scene,
            transform,
            text_renderer,
            theme,
            window,
            &spec,
            wording.body,
        );
    }

    /// Resolve the open question from a keypress or a click on one of its
    /// answers. Always consumes the event: nothing else may act on the file
    /// until the question is answered.
    pub(super) fn handle_question(
        &mut self,
        question: Question,
        event: &WindowEvent,
        scale_factor: f64,
        doc: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        match self.question_sheet.handle(event, scale_factor) {
            Some(SheetResponse::Action(index)) => self.answer(question, index, doc),
            Some(SheetResponse::Dismiss) => self.answer(question, CANCEL, doc),
            None => {}
        }
        *handled = true;
    }

    pub(super) fn answer(
        &mut self,
        question: Question,
        index: usize,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        self.question_sheet.reset();
        match question {
            Question::Conflict => {
                let choice = [
                    ConflictChoice::Reload,
                    ConflictChoice::Overwrite,
                    ConflictChoice::Cancel,
                ]
                .get(index)
                .copied();
                if let Some(choice) = choice {
                    self.resolve_conflict(doc, choice);
                }
            }
            Question::ClosePrompt => {
                let choice = [CloseChoice::Save, CloseChoice::Discard, CloseChoice::Cancel]
                    .get(index)
                    .copied();
                if let Some(choice) = choice {
                    self.resolve_close_prompt(doc, choice);
                }
            }
        }
    }
}
