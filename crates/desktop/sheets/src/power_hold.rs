//! "Log out?" while an app holds work that is not saved.
//!
//! Logging out or shutting down ends the session and every app in it. An app that keeps work the
//! person has not saved says so through the system's inhibitor for it, and this is the question
//! the person answers before that work would go: what is about to happen, which apps would lose
//! it, and the two answers — log out (or shut down) anyway, or stay.
//!
//! The question is the session's to raise and this sheet's to draw, as every other question over
//! the shell. Nothing here ends a session: the effect is the session's own
//! ([`PowerHoldEffect::Carry`] says it may go ahead).

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::{Column, Element, ModalDialog};

use crate::presentation::{Presentation, SheetState, presented};
use crate::{t, t_args, word};

/// The app the session mounts the question as.
pub const APP_ID: &str = "so.tilo.power-hold";

/// The question's resource, where its answers are published. It holds nothing; the effect the
/// person asked for is the session's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct PowerHold {}

#[resource(kind = "power-hold.question")]
impl PowerHold {}

/// What the person answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Effect)]
#[effect(operation = "so.tilo.power-hold.answer")]
pub enum PowerHoldEffect {
    /// Carry out what they asked for: log out or shut down, losing the unsaved work
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Carry,
    /// Leave the session as it is
    #[effect(kind = "read")]
    #[effect(own_device, idempotent)]
    Cancel,
}

/// What the question shows: the effect it is about (`log-out` or `shut-down`), and the apps whose
/// unsaved work would go with it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Question {
    pub effect: String,
    /// What each app says it is holding, in its own words, in the order they were heard.
    pub apps: Vec<String>,
}

impl Question {
    /// The effect's word, as one of the power menu's (`log-out`, `shut-down`).
    #[must_use]
    pub fn effect(&self) -> &str {
        &self.effect
    }

    /// What each app says it is holding, in its own words, in the order they were heard.
    #[must_use]
    pub fn apps(&self) -> &[String] {
        &self.apps
    }

    /// The question's title: what is about to happen.
    #[must_use]
    pub fn title(&self) -> String {
        if self.effect == "shut-down" {
            return word("power-hold-shut-down-title");
        }
        word("power-hold-log-out-title")
    }

    /// What the question says under its title: which apps would lose work, and what happens if
    /// they go ahead.
    #[must_use]
    pub fn body(&self) -> String {
        let apps = self.apps.join(", ");
        t_args(
            if self.effect == "shut-down" {
                "power-hold-shut-down-body"
            } else {
                "power-hold-log-out-body"
            },
            &[("apps", ui::i18n::ArgValue::Text(apps))],
        )
    }

    /// What the answer that goes ahead says.
    #[must_use]
    pub fn carry(&self) -> String {
        if self.effect == "shut-down" {
            return word("power-hold-shut-down-anyway");
        }
        word("power-hold-log-out-anyway")
    }

    /// The answer that stays.
    #[must_use]
    pub fn cancel(&self) -> String {
        t("power-hold-cancel")
    }

    /// Whether it is about anything at all: a question with no effect never stands.
    #[must_use]
    pub fn asked(&self) -> bool {
        !self.effect.is_empty()
    }
}

/// What the session keeps of the sheet between passes: the question it asks, and the answers
/// given since it last asked.
#[derive(Clone, Default)]
pub struct PowerHoldState {
    presentation: Presentation,
    question: Rc<RefCell<Question>>,
    shown: Rc<RefCell<Option<LocalState<Question>>>>,
    asked: Rc<RefCell<Vec<PowerHoldEffect>>>,
}

impl std::fmt::Debug for PowerHoldState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PowerHoldState")
            .field("presentation", &self.presentation)
            .finish_non_exhaustive()
    }
}

impl crate::SheetState for PowerHoldState {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl PowerHoldState {
    /// Put the question up for `question`.
    pub fn ask(&self, question: Question) {
        if *self.question.borrow() == question {
            return;
        }
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.set(question.clone());
        }
        *self.question.borrow_mut() = question;
    }

    /// What it asks now.
    #[must_use]
    pub fn question(&self) -> Question {
        self.question.borrow().clone()
    }

    /// The answers given since the session last asked, oldest first.
    pub fn take_asked(&self) -> Vec<PowerHoldEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    fn answer(&self, effect: PowerHoldEffect) {
        self.asked.borrow_mut().push(effect);
    }
}

/// The question as an app.
pub struct PowerHoldApp;

impl ResourceApp for PowerHoldApp {
    type Resource = PowerHold;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Power";
    const ICON: Option<&'static str> = Some("power");
    // Over the desktop: what is not the question shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<PowerHold>) -> Element {
        let state = cx.take_lent::<PowerHoldState>();
        let initial = match &state {
            Some(state) => state.question(),
            None => cx
                .fixture::<Question>()
                .map(|question| (*question).clone())
                .unwrap_or_default(),
        };
        let shown = cx.local(initial);
        if let Some(state) = &state {
            *state.shown.borrow_mut() = Some(shown.clone());
            state.presentation.wake_with(cx.waker());
        }
        let answering = state.clone();
        cx.effects(move |effect: PowerHoldEffect, _cx| match &answering {
            Some(state) => {
                if effect == PowerHoldEffect::Cancel {
                    // Its own close: the card leaves the screen, and the session tells it gone
                    // once nothing of it is drawn.
                    state.presentation.close();
                }
                state.answer(effect);
                Ok(Effected::done())
            }
            None => Err("no power question is waiting here".to_owned()),
        });
        let dispatch = cx.dispatcher();
        let ask: Ask = Rc::new(move |effect: PowerHoldEffect| dispatch.effect(&effect));
        let presentation = state.as_ref().map(|state| state.presentation().clone());
        cx.switch(&shown, move |_cx, question| {
            card(question, &ask, presentation.as_ref())
        })
    }

    fn scenarios() -> &'static [&'static str] {
        &["log-out", "shut-down"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<PowerHold>) {
        fixture.provide(Question {
            effect: if fixture.scenario() == Some("shut-down") {
                "shut-down".to_owned()
            } else {
                "log-out".to_owned()
            },
            apps: vec![
                "Notes: 2 notes are not saved".to_owned(),
                "Writer: an article is not saved".to_owned(),
            ],
        });
    }
}

/// The question as its card.
fn card(question: &Question, ask: &Ask, presentation: Option<&Presentation>) -> Element {
    let carrying = ask.clone();
    let cancelling = ask.clone();
    let dismissed = ask.clone();
    let mut body = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(ui::tokens::Rhythm::Section);
    body = body.push(Box::new(ui::widget::Text::new(&question.body())));
    Box::new(presented(
        presentation,
        ModalDialog::new(question.title(), body)
            .with_test_id("power-hold")
            .with_icon(Icon::Power)
            .with_description(question.body())
            .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
            // Losing unsaved work is not something Enter presses: the way out is the safe answer,
            // reachable from the keyboard as from everything else.
            .with_destructive(true)
            .primary_action(question.carry(), move || {
                carrying(PowerHoldEffect::Carry);
            })
            .secondary_action(question.cancel(), move || {
                cancelling(PowerHoldEffect::Cancel);
            })
            .on_dismiss(move || {
                dismissed(PowerHoldEffect::Cancel);
            }),
    ))
}

/// How the card asks the person for an answer.
pub type Ask = Rc<dyn Fn(PowerHoldEffect)>;

/// The sheet's definition, as the session mounts it.
#[must_use]
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(PowerHoldApp)
}

/// The words this sheet uses, as a capture or a test reads them.
#[must_use]
pub fn sample(effect: &str) -> Question {
    Question {
        effect: effect.to_owned(),
        apps: vec![
            "Notes: 2 notes are not saved".to_owned(),
            "Writer: an article is not saved".to_owned(),
        ],
    }
}
