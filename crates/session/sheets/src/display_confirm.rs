//! Keep or revert a risky display change a tool made.
//!
//! A layout tool (`kanshi`, `wlr-randr`) stages the same display transaction
//! Settings does. One of its changes that could leave a screen blank or turn
//! off the one in use waits for the person, and the previous layout comes
//! back unless they keep it. The question stands on every display at once,
//! so it is seen even when the new layout blanked one of them: the title,
//! the countdown, Keep and Revert, and for a known tool a third answer that
//! keeps the change and stops asking about that tool. Enter keeps (Keep has
//! the keyboard), Escape reverts, and running out of time reverts.
//!
//! Keeping a change is the person's answer: whoever acts for them is always
//! asked first. Reverting is safe for anyone. The shell holds the
//! transaction and records the answer ([`ConfirmSeat`]).

use std::cell::RefCell;
use std::rc::Rc;

use ui::advanced::WindowEvent;
use ui::framework::{Dispatcher, Effected, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::{
    Button, ButtonRole, Column, Element, FloatingPanel, Manual, MeasureConstraint, Row, Text,
    Widget,
};

use crate::presentation::Presentation;
use crate::t;

/// The app the shell mounts the question as.
pub const APP_ID: &str = "so.tilo.display-confirm";

/// The question's resource: the door its answers are published on. It holds
/// nothing; the transaction is the display backend's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DisplayConfirm {}

#[resource(kind = "display-confirm.question", label = "Display change")]
impl DisplayConfirm {}

/// The answers to the question.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Effect)]
#[effect(operation = "so.tilo.display-confirm.answer")]
pub enum ConfirmEffect {
    /// Keep the display change
    #[effect(own_device, attended)]
    Keep,
    /// Revert the display change
    #[effect(own_device, idempotent)]
    Revert,
    /// Keep the display change and stop asking about its tool
    #[effect(own_device, attended)]
    KeepAndTrust,
}

/// What the question shows: plain data the shell keeps current.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Question {
    /// The tool that made the change, when it is known.
    pub tool: Option<String>,
    /// The countdown, or what became of the answer.
    pub status: String,
    /// An answer is being recorded: the buttons wait.
    pub answering: bool,
    /// Every display, in the sheet's own coordinates.
    pub displays: Vec<Rect>,
}

impl Question {
    fn title(&self) -> String {
        self.tool.as_ref().map_or_else(
            || t("display-confirm-tool-unknown-title"),
            |tool| {
                crate::t_args(
                    "display-confirm-tool-title",
                    &[("tool", ui::i18n::ArgValue::Text(tool.clone()))],
                )
            },
        )
    }
}

/// What the cards show: the question and the answer the keyboard is on.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shown {
    question: Question,
    /// Index into [`Question::answers`].
    focus: usize,
}

impl Question {
    /// The answers in the order the keyboard and a reader meet them.
    fn answers(&self) -> Vec<ConfirmEffect> {
        let mut answers = vec![ConfirmEffect::Keep, ConfirmEffect::Revert];
        if self.tool.is_some() {
            answers.push(ConfirmEffect::KeepAndTrust);
        }
        answers
    }
}

/// What the shell lends the question: whether it is up, what it asks, and
/// where the answer goes.
#[derive(Clone, Default)]
pub struct ConfirmSeat {
    presentation: Presentation,
    question: Rc<RefCell<Question>>,
    shown: Rc<RefCell<Option<LocalState<Shown>>>>,
    asked: Rc<RefCell<Vec<ConfirmEffect>>>,
}

impl std::fmt::Debug for ConfirmSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmSeat")
            .field("presentation", &self.presentation)
            .field("question", &self.question.borrow())
            .finish_non_exhaustive()
    }
}

impl crate::Seat for ConfirmSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl ConfirmSeat {
    /// Show `question`, or the same question as it stands now.
    pub fn ask(&self, question: Question) {
        if *self.question.borrow() == question {
            return;
        }
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.update(|shown| shown.question = question.clone());
        }
        *self.question.borrow_mut() = question;
    }

    /// What the question asks now.
    pub fn question(&self) -> Question {
        self.question.borrow().clone()
    }

    /// The answers given since the shell last asked, oldest first.
    pub fn take_asked(&self) -> Vec<ConfirmEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    fn answer(&self, effect: ConfirmEffect) {
        self.asked.borrow_mut().push(effect);
    }
}

/// The question as an app.
pub struct DisplayConfirmApp;

impl ResourceApp for DisplayConfirmApp {
    type Resource = DisplayConfirm;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Display change";
    const ICON: Option<&'static str> = Some("display");
    // Over the desktop: what is not a card shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<DisplayConfirm>) -> Element {
        let seat = cx.take_lent::<ConfirmSeat>();
        let initial = match &seat {
            Some(seat) => seat.question.borrow().clone(),
            None => cx
                .fixture::<Question>()
                .map(|question| (*question).clone())
                .unwrap_or_default(),
        };
        let shown = cx.local(Shown {
            question: initial,
            focus: 0,
        });
        if let Some(seat) = &seat {
            *seat.shown.borrow_mut() = Some(shown.clone());
        }
        let answering = seat;
        cx.effects(move |effect: ConfirmEffect, _cx| match &answering {
            Some(seat) => {
                seat.answer(effect);
                Ok(Effected::done())
            }
            None => Err("no display change is waiting here".to_owned()),
        });
        let dispatch = cx.dispatcher();
        let cards = cx.switch(&shown, cards);
        Box::new(Answering {
            content: cards,
            dispatch,
            shown,
        })
    }

    fn scenarios() -> &'static [&'static str] {
        &["known-tool", "unknown-tool", "two-displays", "answering"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<DisplayConfirm>) {
        let one = vec![Rect::new(0.0, 0.0, 1280.0, 800.0)];
        let question = match fixture.scenario() {
            Some("unknown-tool") => Question {
                tool: None,
                status: "Reverting in 12 s".to_owned(),
                answering: false,
                displays: one,
            },
            Some("two-displays") => Question {
                tool: Some("kanshi".to_owned()),
                status: "Reverting in 12 s".to_owned(),
                answering: false,
                displays: vec![
                    Rect::new(0.0, 0.0, 640.0, 800.0),
                    Rect::new(640.0, 0.0, 1280.0, 800.0),
                ],
            },
            Some("answering") => Question {
                tool: Some("kanshi".to_owned()),
                status: "Saving the display answer…".to_owned(),
                answering: true,
                displays: one,
            },
            _ => Question {
                tool: Some("kanshi".to_owned()),
                status: "Reverting in 12 s".to_owned(),
                answering: false,
                displays: one,
            },
        };
        fixture.provide(question);
    }
}

/// One card per display, centred on it.
fn cards(cx: &mut ViewCx<DisplayConfirm>, shown: &Shown) -> Element {
    let question = &shown.question;
    let displays = question.displays.clone();
    let mut manual = Manual::new(move |layout| {
        let edge = f64::from(ui::tokens::Spacing::BASE.s4);
        let metrics = ui::tokens::Metrics::BASE;
        for (index, display) in displays.iter().enumerate().take(layout.len()) {
            let width = f64::from(metrics.panel_width_sheet)
                .min(display.width() - edge * 2.0)
                .max(f64::from(metrics.panel_width_utility));
            let measured = layout.measure(
                index,
                ui::taffy::Size {
                    width: MeasureConstraint::Exact(ui::num::f32_of_f64(width)),
                    height: MeasureConstraint::Intrinsic,
                },
            )?;
            let card =
                Rect::from_center_size(display.center(), (width, f64::from(measured.size.height)));
            layout.place(index, card)?;
        }
        Ok(layout.available())
    });
    for (index, _) in question.displays.iter().enumerate() {
        // Only the first card has the keyboard.
        let focus = (index == 0).then_some(shown.focus);
        let card = card(cx, question, index, focus);
        // The other displays show the same question for the eye: the
        // keyboard and assistive technology meet it once.
        manual = manual.child(if index == 0 {
            card
        } else {
            Box::new(Mirror(card))
        });
    }
    Box::new(manual)
}

/// The question on display `index`, with the keyboard on answer `focus`
/// of [`Question::answers`] when it has it.
fn card(
    cx: &ViewCx<DisplayConfirm>,
    question: &Question,
    index: usize,
    focus: Option<usize>,
) -> Element {
    let actions = cx.actions();
    let focused = focus.and_then(|focus| question.answers().get(focus).copied());
    let button = |effect: ConfirmEffect, id: &str, label: &str| {
        let mut button =
            Button::new(&format!("{id}-{index}"), label).with_enabled(!question.answering);
        button.set_focus(focused == Some(effect));
        match actions.effect(&effect) {
            Some(binding) => button.on_action(binding),
            None => button,
        }
    };
    let keep = button(
        ConfirmEffect::Keep,
        "display-confirm-keep",
        &t("display-confirm-keep"),
    )
    .role(ButtonRole::Primary);
    let revert = button(
        ConfirmEffect::Revert,
        "display-confirm-revert",
        &t("display-confirm-revert"),
    );
    let answers = Row::new()
        .with_gap(ui::tokens::Spacing::BASE.s2)
        .push(Box::new(keep))
        .push(Box::new(revert));
    // Keep and Revert first, then the answer that also stops the asking:
    // the order the keyboard and a reader meet them.
    let mut footer = Column::new()
        .with_gap(ui::tokens::Spacing::BASE.s2)
        .push(Box::new(answers));
    if let Some(tool) = &question.tool {
        let label = crate::t_args(
            "display-confirm-always",
            &[("tool", ui::i18n::ArgValue::Text(tool.clone()))],
        );
        footer = footer.push(Box::new(button(
            ConfirmEffect::KeepAndTrust,
            "display-confirm-always",
            &label,
        )));
    }
    let body = Column::new().push(Box::new(
        Text::new(&question.status).with_role(TextRole::Caption),
    ));
    Box::new(
        FloatingPanel::new(Box::new(body))
            .header(Box::new(
                Text::new(&question.title()).with_role(TextRole::Subheading),
            ))
            .footer(Box::new(footer)),
    )
}

/// Escape reverts from anywhere on the question; Tab walks its answers and
/// Enter or Space gives the one with the keyboard, Keep until the person
/// moves on, as the question says. The question is modal and keeps the
/// keyboard to itself, as a dialog does.
struct Answering {
    content: Element,
    dispatch: Dispatcher<DisplayConfirm>,
    shown: LocalState<Shown>,
}

impl ui::widget::Decorator for Answering {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    /// The question walks its own answers: Tab is its, not the host's.
    fn wants_tab_input(&self) -> bool {
        true
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        use ui::winit::keyboard::{Key, NamedKey};
        let pressed = match event {
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ui::winit::event::ElementState::Pressed =>
            {
                Some(key.logical_key.clone())
            }
            _ => None,
        };
        let Some(Key::Named(key)) = pressed else {
            self.content.handle_event(event, ctx);
            return;
        };
        let Shown { question, focus } = self.shown.get();
        let answers = question.answers();
        match key {
            NamedKey::Escape => self.dispatch.effect(&ConfirmEffect::Revert),
            NamedKey::Tab => {
                let count = answers.len();
                let next = if ctx.modifiers.state().shift_key() {
                    focus
                        .checked_sub(1)
                        .unwrap_or_else(|| count.saturating_sub(1))
                } else {
                    focus
                        .checked_add(1)
                        .filter(|&next| next < count)
                        .unwrap_or(0)
                };
                self.shown.update(|shown| shown.focus = next);
            }
            NamedKey::Enter | NamedKey::Space => {
                if !question.answering {
                    let answer = answers.get(focus).copied().unwrap_or(ConfirmEffect::Keep);
                    self.dispatch.effect(&answer);
                }
            }
            _ => {
                self.content.handle_event(event, ctx);
                return;
            }
        }
        *ctx.handled = true;
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let question = self.shown.get().question;
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(question.title())
                .with_description(format!(
                    "{}. {}",
                    question.status,
                    t("display-confirm-a11y-description")
                ))
                .with_state(AccessState::new().modal(true)),
        )
    }
}

/// A copy of the question on another display: seen, pressed with the
/// pointer, and out of the focus order and the accessibility tree.
struct Mirror(Element);

impl ui::widget::Decorator for Mirror {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.0.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.0.as_mut()
    }

    fn accessibility_hidden(&self) -> bool {
        true
    }

    /// The copy stands for its card in the tree, so nothing under it is
    /// published a second time.
    fn accessibility_represents_child(&self, _child_index: usize) -> bool {
        true
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        Some(AccessNode::new(Role::Group).with_state(AccessState {
            hidden: true,
            ..AccessState::default()
        }))
    }
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(DisplayConfirmApp)
}
