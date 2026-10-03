//! The editor's two questions: "this file changed on disk" and "save changes
//! before closing?". Both are decisions the user makes — silently choosing for
//! them loses somebody's work — and both have three answers, so they are the
//! toolkit's modal sheet ([`ui::widget::modal_dialog`]) with three footer
//! actions rather than a retained two-action dialog.
//!
//! The sheet is painted in the window's top layer ([`Widget::draw_overlay`]),
//! over the whole window, so its scrim covers the sidebar and the tabs too.

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::modal_dialog;
use ui::widget::{ModalAction, ModalSpec};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::{CloseChoice, CodeApp, ConflictChoice};

/// Which question is on screen. A conflict outranks a close prompt: it is the
/// reason a save did not land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Question {
    Conflict,
    ClosePrompt,
}

/// What one question says, in the order its actions stand.
struct Wording {
    title: String,
    body: &'static str,
    actions: [ModalAction<'static>; 3],
}

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
                    ModalAction::primary("Reload").focused(true),
                    ModalAction::danger("Overwrite"),
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
                    ModalAction::primary("Save").focused(true),
                    ModalAction::danger("Don't save"),
                    ModalAction::secondary("Cancel"),
                ],
            },
        }
    }

    /// Paint the open question over the whole `window`, and remember where its
    /// actions landed for hit-testing.
    pub(super) fn draw_question(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        self.dialog_actions.clear();
        let Some(question) = self.question() else {
            return;
        };
        let wording = self.wording(question);
        let hovered = self.dialog_hover;
        let actions: Vec<ModalAction<'_>> = wording
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| action.hovered(hovered == Some(index)))
            .collect();
        let body_style = theme.text(TextRole::Body);
        let max_width = modal_dialog::MODAL_MAX_WIDTH;
        let body_width = modal_dialog::body_width(window, max_width, theme);
        let (_, body_height) =
            text_renderer.measure_style(wording.body, Some(body_width as f32), &body_style);
        let spec = ModalSpec::new(&wording.title)
            .actions(&actions)
            .body_height(f64::from(body_height))
            .max_width(max_width)
            .closable(false)
            .dismiss_on_scrim(false);
        let layout = modal_dialog::layout(window, &spec, text_renderer, theme);
        modal_dialog::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);
        let (_, dim) = modal_dialog::ink(theme);
        text_renderer.draw_style(
            scene,
            wording.body,
            dim,
            transform.pre_translate(Vec2::new(layout.body.x0, layout.body.y0)),
            Some(layout.body.width() as f32),
            &body_style,
        );
        modal_dialog::paint_actions(scene, transform, text_renderer, theme, &layout, &spec);
        self.dialog_actions = layout.actions;
    }

    /// Resolve the open question from a keypress or a click on one of its
    /// actions. Always consumes the event: nothing else may act on the file
    /// until the question is answered.
    pub(super) fn handle_question(
        &mut self,
        question: Question,
        event: &WindowEvent,
        doc: &mut automerge::AutoCommit,
        handled: &mut bool,
    ) {
        match event {
            WindowEvent::CursorMoved { .. } => {
                let over = self
                    .dialog_actions
                    .iter()
                    .position(|rect| rect.area() > 0.0 && rect.contains(self.last_cursor));
                self.dialog_hover = over;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(index) = self.dialog_hover_at(self.last_cursor) {
                    self.answer(question, index, doc);
                }
            }
            _ => {
                if let Some(index) = Self::question_key(question, event) {
                    self.answer(question, index, doc);
                }
            }
        }
        *handled = true;
    }

    fn dialog_hover_at(&self, point: Point) -> Option<usize> {
        self.dialog_actions
            .iter()
            .position(|rect| rect.area() > 0.0 && rect.contains(point))
    }

    /// The action a keypress stands for: Escape cancels, Enter takes the
    /// default (the one that cannot destroy anyone's work).
    fn question_key(question: Question, event: &WindowEvent) -> Option<usize> {
        match question {
            Question::Conflict => Self::conflict_key(event).map(|choice| match choice {
                ConflictChoice::Reload => 0,
                ConflictChoice::Overwrite => 1,
                ConflictChoice::Cancel => 2,
            }),
            Question::ClosePrompt => Self::close_prompt_key(event).map(|choice| match choice {
                CloseChoice::Save => 0,
                CloseChoice::Discard => 1,
                CloseChoice::Cancel => 2,
            }),
        }
    }

    pub(super) fn answer(
        &mut self,
        question: Question,
        index: usize,
        doc: &mut automerge::AutoCommit,
    ) {
        self.dialog_hover = None;
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
