//! The editor's questions: "this file changed on disk", "save changes before
//! closing?", and why code help is unavailable. The first two are decisions
//! the user makes — silently choosing for them loses somebody's work. They are
//! the toolkit's immediate modal sheet ([`ModalSheet`]): it paints, hit-tests
//! and answers, the editor says what is asked.
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
    CodeHelp,
}

/// What one question says, in the order its answers are indexed: the safe
/// default first, the answer that changes nothing last.
pub(super) struct Wording {
    pub(super) title: String,
    pub(super) body: String,
    pub(super) actions: Vec<Answer>,
    /// Whether a press beside the sheet or its close button may answer it
    /// with the last answer: only when nothing is at stake.
    pub(super) dismissable: bool,
}

/// One answer of a question, as its button says it.
pub(super) struct Answer {
    label: String,
    weight: Weight,
}

#[derive(Clone, Copy)]
enum Weight {
    Primary,
    Secondary,
    Discards,
}

impl Answer {
    pub(super) fn primary(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            weight: Weight::Primary,
        }
    }

    pub(super) fn secondary(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            weight: Weight::Secondary,
        }
    }

    fn discards(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            weight: Weight::Discards,
        }
    }

    fn action(&self) -> ModalAction<'_> {
        match self.weight {
            Weight::Primary => ModalAction::primary(&self.label),
            Weight::Secondary => ModalAction::secondary(&self.label),
            Weight::Discards => ModalAction::alternate_danger(&self.label),
        }
    }
}

impl CodeApp {
    /// The question waiting for an answer, if any.
    pub(super) fn question(&self) -> Option<Question> {
        if self.conflict.is_some() {
            Some(Question::Conflict)
        } else if self.close_prompt.is_some() {
            Some(Question::ClosePrompt)
        } else if self.code_help_asked() {
            Some(Question::CodeHelp)
        } else {
            None
        }
    }

    pub(super) fn wording(&self, question: Question) -> Wording {
        match question {
            Question::Conflict => Wording {
                title: format!(
                    "{} changed on disk",
                    self.conflict.as_ref().map_or("", |c| c.name.as_str())
                ),
                body: "Something else wrote this file after you opened it. Overwriting \
                       discards their change; reloading discards yours."
                    .to_owned(),
                actions: vec![
                    Answer::primary("Reload"),
                    Answer::discards("Overwrite"),
                    Answer::secondary("Cancel"),
                ],
                dismissable: false,
            },
            Question::ClosePrompt => Wording {
                title: format!(
                    "Save changes to {}?",
                    self.close_prompt.as_ref().map_or("", |p| p.name.as_str())
                ),
                body: "This file has unsaved changes. If you don't save them, they will \
                       be lost."
                    .to_owned(),
                actions: vec![
                    Answer::primary("Save"),
                    Answer::discards("Don't save"),
                    Answer::secondary("Cancel"),
                ],
                dismissable: false,
            },
            Question::CodeHelp => self.code_help_wording(),
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
        let actions: Vec<ModalAction<'_>> = wording.actions.iter().map(Answer::action).collect();
        let spec = ModalSpec::new(&wording.title)
            .actions(&actions)
            .closable(wording.dismissable)
            .dismiss_on_scrim(wording.dismissable);
        self.question_sheet.paint_message(
            scene,
            transform,
            text_renderer,
            theme,
            window,
            &spec,
            &wording.body,
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
            Some(SheetResponse::Dismiss) => {
                let last = self.wording(question).actions.len().saturating_sub(1);
                self.answer(question, last, doc);
            }
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
            Question::CodeHelp => self.answer_code_help(index),
        }
    }
}
