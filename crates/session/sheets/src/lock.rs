//! The lock screen: the session's own, shown on the first display while
//! the session is locked, over nothing (the compositor draws no window or
//! other surface then, and the other displays stay dark).
//!
//! It shows the time, who the session belongs to, and the one password
//! prompt ([`SecretPrompt`]). Typing the password and unlocking are the
//! app's effects, so a person, an assistive tool and a headless run reach
//! the same door. The password never leaves the seat's zeroizing buffer
//! except to the session, which verifies it ([`LockSeat::take_attempts`])
//! and either lifts the lock or says it was wrong ([`LockSeat::refuse`]).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ui::advanced::WindowEvent;
use ui::framework::{Dispatcher, Effected, ResourceApp, ViewCx};
use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::{
    Column, Element, Manual, MeasureConstraint, Presentation as PromptPresentation, PromptOutcome,
    SecretKind, SecretPrompt, Text, Widget,
};
use zeroize::{Zeroize, Zeroizing};

use crate::Seat as _;
use crate::presentation::Presentation;
use crate::t;

/// The app the session mounts the lock screen as.
pub const APP_ID: &str = "so.tilo.lock";

/// The prompt's id, which the view finds it by.
const PROMPT_ID: &str = "lock-password";

/// The lock screen's resource: the door unlocking is published on. It holds
/// nothing; the lock is the session's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct LockScreen {}

#[resource(kind = "lock.screen", label = "Lock screen")]
impl LockScreen {}

/// What can be done on the lock screen.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Effect)]
#[effect(operation = "so.tilo.lock.screen")]
pub enum LockEffect {
    /// Type the password
    #[effect(own_device)]
    TypePassword {
        #[param(secret, "The password field's whole value")]
        password: String,
    },
    /// Unlock with the password typed
    #[effect(own_device)]
    Unlock,
}

/// What the lock screen shows: plain data the session keeps current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    /// The time, as the person reads clocks.
    pub clock: String,
    /// Whose session this is.
    pub person: String,
    /// Why the last password did not unlock.
    pub error: Option<String>,
    /// The password is being checked.
    pub checking: bool,
}

/// What the session lends the lock screen: whether it is up, what it
/// shows, the password field's buffer and the attempts to unlock.
#[derive(Clone, Default)]
pub struct LockSeat {
    presentation: Presentation,
    shown: Rc<RefCell<Shown>>,
    local: Rc<RefCell<Option<LocalState<Shown>>>>,
    secret: Arc<Mutex<String>>,
    attempts: Rc<RefCell<Vec<Zeroizing<String>>>>,
}

impl std::fmt::Debug for LockSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockSeat")
            .field("presentation", &self.presentation)
            .field("shown", &self.shown.borrow())
            .finish_non_exhaustive()
    }
}

impl crate::Seat for LockSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl LockSeat {
    /// Show `shown`, when it is not what shows already.
    pub fn show(&self, shown: Shown) {
        if *self.shown.borrow() == shown {
            return;
        }
        if let Some(local) = self.local.borrow().as_ref() {
            local.update(|now| *now = shown.clone());
        }
        *self.shown.borrow_mut() = shown;
        self.presentation.wake();
    }

    /// What the lock screen shows now.
    pub fn shown(&self) -> Shown {
        self.shown.borrow().clone()
    }

    /// The passwords given to unlock since the session last asked, oldest
    /// first; each zeroes itself when dropped.
    pub fn take_attempts(&self) -> Vec<Zeroizing<String>> {
        std::mem::take(&mut *self.attempts.borrow_mut())
    }

    /// The password did not unlock: say `why` and let the person try
    /// again.
    pub fn refuse(&self, why: String) {
        self.show(Shown {
            error: Some(why),
            checking: false,
            ..self.shown()
        });
    }

    /// The lock is lifted: forget what was typed and what was said.
    pub fn lifted(&self) {
        self.wipe();
        self.show(Shown {
            error: None,
            checking: false,
            ..self.shown()
        });
    }

    fn type_password(&self, password: &str) {
        if let Ok(mut held) = self.secret.lock() {
            held.zeroize();
            held.push_str(password);
        }
    }

    /// Hand what the field holds to the session; an empty field gives
    /// nothing.
    fn attempt(&self) -> Result<(), String> {
        let password = self
            .secret
            .lock()
            .map(|held| Zeroizing::new(held.clone()))
            .map_err(|_| t("lock-no-password"))?;
        if password.is_empty() {
            return Err(t("lock-no-password"));
        }
        self.wipe();
        self.attempts.borrow_mut().push(password);
        self.show(Shown {
            error: None,
            checking: true,
            ..self.shown()
        });
        Ok(())
    }

    fn wipe(&self) {
        if let Ok(mut held) = self.secret.lock() {
            held.zeroize();
        }
    }
}

/// What the lock screen says when a password did not unlock.
#[must_use]
pub fn wrong_password() -> String {
    t("lock-wrong-password")
}

/// What it says when the password could not be checked.
#[must_use]
pub fn unavailable() -> String {
    t("lock-unavailable")
}

/// The lock screen as an app.
pub struct LockApp;

impl ResourceApp for LockApp {
    type Resource = LockScreen;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Lock screen";
    const ICON: Option<&'static str> = Some("lock");
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<LockScreen>) -> Element {
        let seat = cx.take_lent::<LockSeat>().unwrap_or_default();
        let initial = cx
            .fixture::<Shown>()
            .map_or_else(|| seat.shown(), |shown| (*shown).clone());
        let shown = cx.local(initial);
        *seat.local.borrow_mut() = Some(shown.clone());
        seat.presentation().wake_with(cx.waker());
        let acting = seat.clone();
        cx.effects(move |effect: LockEffect, _cx| {
            match effect {
                LockEffect::TypePassword { password } => {
                    let password = Zeroizing::new(password);
                    acting.type_password(&password);
                }
                LockEffect::Unlock => acting.attempt()?,
            }
            Ok(Effected::done())
        });
        let dispatch = cx.dispatcher();
        let content = cx.switch(&shown, move |_cx, shown| screen(shown, &seat));
        Box::new(Locked { content, dispatch })
    }

    fn scenarios() -> &'static [&'static str] {
        &["plain", "refused", "checking"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<LockScreen>) {
        let shown = Shown {
            clock: "10:42".to_owned(),
            person: "Ada Lovelace".to_owned(),
            error: None,
            checking: false,
        };
        let shown = match fixture.scenario() {
            Some("refused") => Shown {
                error: Some(t("lock-wrong-password")),
                ..shown
            },
            Some("checking") => Shown {
                checking: true,
                ..shown
            },
            _ => shown,
        };
        fixture.provide(shown);
    }
}

/// The time, whose session it is, and the password prompt, centred.
fn screen(shown: &Shown, seat: &LockSeat) -> Element {
    let mut prompt = SecretPrompt::new(SecretKind::Secret)
        .with_id(PROMPT_ID)
        .with_presentation(PromptPresentation::Inline)
        .with_icon(Icon::Lock)
        .with_resource(shown.person.clone())
        .with_primary_label(t("lock-unlock"))
        .with_secret_buffer(Arc::clone(&seat.secret))
        .with_error(shown.error.clone())
        .with_busy(shown.checking.then(|| t("lock-checking")))
        .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_utility));
    Widget::set_focus(&mut prompt, true);
    let column = Column::new()
        .with_gap(ui::tokens::Spacing::BASE.s4)
        .push(Box::new(
            Text::new(&shown.clock).with_role(TextRole::Display),
        ))
        .push(Box::new(prompt));
    // Centred on the screen.
    Box::new(
        Manual::new(|layout| {
            let available = layout.available();
            let area = Rect::new(
                0.0,
                0.0,
                f64::from(available.width),
                f64::from(available.height),
            );
            let width = f64::from(ui::tokens::Metrics::BASE.panel_width_utility).min(area.width());
            let measured = layout.measure(
                0,
                ui::taffy::Size {
                    width: MeasureConstraint::Exact(ui::num::f32_of_f64(width)),
                    height: MeasureConstraint::Intrinsic,
                },
            )?;
            let size = (width, f64::from(measured.size.height));
            layout.place(0, Rect::from_center_size(area.center(), size))?;
            Ok(available)
        })
        .child(Box::new(column)),
    )
}

/// The lock screen keeps the keyboard: what the prompt answers goes to the
/// door as the effects anyone else would run.
struct Locked {
    content: Element,
    dispatch: Dispatcher<LockScreen>,
}

impl Locked {
    fn settle_prompt(&mut self) {
        fn find(widget: &mut dyn Widget) -> Option<&mut SecretPrompt> {
            if widget.test_id() == Some(PROMPT_ID) {
                return widget
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<SecretPrompt>());
            }
            widget.children_mut().into_iter().find_map(find)
        }
        let Some(prompt) = find(self.content.as_mut()) else {
            return;
        };
        // The answer's password is the buffer's, which the effect reads.
        if let Some(PromptOutcome::Submit(_)) = prompt.take_outcome() {
            self.dispatch.effect(&LockEffect::Unlock);
        }
    }
}

impl ui::widget::Decorator for Locked {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.content.handle_event(event, ctx);
        self.settle_prompt();
    }

    fn traps_focus(&self) -> bool {
        true
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(t("lock-title"))
                .with_state(AccessState::new().modal(true)),
        )
    }
}

/// The app's definition, which the session mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(LockApp)
}
