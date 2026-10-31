//! The question that starts a dictation, and what it shows while one runs.
//!
//! An app that offers dictation in a field asks the session to run one
//! there. The session mounts this sheet for the answer: the microphone is
//! allowed for one dictation, for the field the person was in, and nothing
//! is heard before they allow it. While the dictation runs the sheet shows
//! what is being heard, so the person can stop it and keep the words, or
//! throw them away; a transcript the field would not take stays here to copy
//! instead of being lost.
//!
//! Allowing the microphone is the person's own answer, so it is a press on
//! this card and nothing else: the buttons answer the session's seat
//! directly ([`DictationSeat::take_answers`]). The sheet publishes no
//! effect vocabulary for them, and none for the dictation's other answers
//! either — an effect is reachable through the session's action door, and a
//! remote action is not consent.
//!
//! The dictation itself — the recognizer, the field, the clipboard — is the
//! session's. The sheet holds what is shown ([`Prompt`]) and what was
//! answered ([`Answer`]).

use std::cell::RefCell;
use std::rc::Rc;

use ui::advanced::WindowEvent;
use ui::framework::{ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::{
    Button, ButtonRole, Column, Element, FloatingPanel, Manual, MeasureConstraint, Row, Text,
    Widget,
};

use crate::presentation::Presentation;
use crate::{t, words as words_with};

/// The app the session mounts the dictation sheet as.
pub const APP_ID: &str = "so.tilo.dictation";

/// The stage a dictation is in, as the sheet shows it: one for each step
/// between the person's key and the words in their field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    /// Waiting for the person to allow the microphone for this dictation.
    #[default]
    Asking,
    /// Waiting for the field to ask for typing.
    Preparing,
    /// Starting the local recognizer; the microphone is off.
    Loading,
    /// Recording: what is heard follows in `Prompt::draft`.
    Listening,
    /// Stopping the microphone.
    StoppingMicrophone,
    /// The text is being finished.
    Finishing,
    /// Stopping what was left of the dictation.
    Stopping,
    /// Nothing was inserted; the transcript can be copied.
    Recovery,
    /// The dictation could not run, or what it heard could not be inserted.
    Unavailable,
}

/// Why a transcript was kept instead of inserted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryReason {
    /// The person changed their personal dictionary while dictating.
    VocabularyChanged,
    /// The transcript was longer than a field takes at once.
    TooLong,
}

/// Why a dictation stopped, as the card says it. The session names the
/// reason; the words are the sheet's, in the person's language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// No field had the focus, or the field takes a secret.
    Field,
    /// The field is a terminal's, where spoken words would be commands.
    UnsupportedField,
    /// The dictation before this one still stopped.
    Busy,
    /// The field the person typed into is no longer the one it was asked
    /// about.
    TargetChanged,
    /// The dictation ran past its window.
    TimedOut,
    /// No local speech model is set up.
    ModelRequired,
    /// The model does not recognise the person's language.
    LanguageUnavailable,
    /// The microphone could not be opened.
    MicrophoneUnavailable,
    /// The person's dictionary changed or went away.
    VocabularyUnavailable,
    /// The microphone's audio broke off.
    AudioInterrupted,
    /// Anything else: local dictation is unavailable.
    Unavailable,
}

impl Failure {
    /// The card's words for it.
    fn key(self) -> &'static str {
        match self {
            Self::Field => "dictation-editable-required",
            Self::UnsupportedField => "dictation-unsupported-field",
            Self::Busy => "dictation-busy",
            Self::TargetChanged => "dictation-target-changed",
            Self::TimedOut => "dictation-timeout",
            Self::ModelRequired => "dictation-model-required",
            Self::LanguageUnavailable => "dictation-language-unavailable",
            Self::MicrophoneUnavailable => "dictation-microphone-unavailable",
            Self::VocabularyUnavailable => "dictation-vocabulary-unavailable",
            Self::AudioInterrupted => "dictation-audio-interrupted",
            Self::Unavailable => "dictation-unavailable",
        }
    }
}

/// What the session shows about the dictation it runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prompt {
    /// The dictation session this is: answers name it, so a retired prompt
    /// cannot answer its successor.
    pub id: u64,
    pub stage: Stage,
    /// The live hypothesis while recording, already cut to what the card
    /// shows; empty otherwise.
    pub draft: String,
    /// What went wrong, when the dictation could not run or was not inserted.
    pub failure: Option<Failure>,
    /// Why the transcript is kept, while `stage` is `Recovery`.
    pub recovery: Option<RecoveryReason>,
    /// The kept transcript reached the clipboard.
    pub copied: bool,
    /// The clipboard could not take the kept transcript.
    pub copy_failed: bool,
}

/// What the person answered, once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Record once, for this field, with the microphone.
    Allow(u64),
    /// Do not record.
    Deny(u64),
    /// Stop recording and keep what was heard.
    Finish(u64),
    /// Stop and throw away what was heard.
    Cancel(u64),
    /// Copy the kept transcript to the clipboard.
    Copy(u64),
    /// Throw the kept transcript away.
    Discard(u64),
}

/// The sheet's resource: what it shows. It holds only what is shown; the
/// dictation itself is the session's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Dictation {}

#[resource(kind = "dictation.session", label = "Dictation")]
impl Dictation {}

/// What the card shows: the prompt, as the session last said it stands.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shown {
    prompt: Option<Prompt>,
}

/// What the session lends the sheet: what to show, and where the answer goes.
#[derive(Clone, Default)]
pub struct DictationSeat {
    presentation: Presentation,
    prompt: Rc<RefCell<Option<Prompt>>>,
    shown: Rc<RefCell<Option<LocalState<Shown>>>>,
    answers: Rc<RefCell<Vec<Answer>>>,
}

impl std::fmt::Debug for DictationSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DictationSeat")
            .field("presentation", &self.presentation)
            .field("prompt", &self.prompt.borrow())
            .finish_non_exhaustive()
    }
}

impl crate::Seat for DictationSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl DictationSeat {
    /// Show `prompt`; `None` when there is nothing to show.
    pub fn show(&self, prompt: Option<Prompt>) {
        if *self.prompt.borrow() == prompt {
            return;
        }
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.update(|shown| shown.prompt.clone_from(&prompt));
        }
        *self.prompt.borrow_mut() = prompt;
        // The card is up already and was woken by its own state; a card not
        // built yet finds the prompt when it is.
        self.presentation.wake();
    }

    /// What the person answered since the session last asked, oldest first.
    pub fn take_answers(&self) -> Vec<Answer> {
        std::mem::take(&mut *self.answers.borrow_mut())
    }

    /// What the sheet shows now.
    pub fn prompt(&self) -> Option<Prompt> {
        self.prompt.borrow().clone()
    }

    /// The person answered from the card. Only the card calls this: the
    /// seat is the sheet's own, and no action reaches it.
    fn answer(&self, answer: Answer) {
        self.answers.borrow_mut().push(answer);
        self.presentation.wake();
    }
}

/// The dictation as an app.
pub struct DictationApp;

impl ResourceApp for DictationApp {
    type Resource = Dictation;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Dictation";
    const ICON: Option<&'static str> = Some("mic");
    // Over the field: what is not a card shows what the person was doing.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<Dictation>) -> Element {
        let seat = cx.take_lent::<DictationSeat>();
        let initial = match &seat {
            Some(seat) => seat.prompt(),
            None => cx
                .fixture::<Prompt>()
                .map(|prompt| Some((*prompt).clone()))
                .unwrap_or_default(),
        };
        let shown = cx.local(Shown { prompt: initial });
        if let Some(seat) = &seat {
            *seat.shown.borrow_mut() = Some(shown.clone());
        }
        let lent = seat.clone();
        let cards = cx.switch(&shown, move |_cx, shown| card(shown, lent.as_ref()));
        Box::new(Answering {
            content: cards,
            shown,
            seat,
        })
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "asking",
            "preparing",
            "loading",
            "listening",
            "stopping-microphone",
            "finishing",
            "recovery-too-long",
            "recovery-vocabulary",
            "recovery-copied",
            "recovery-copy-failed",
            "unavailable",
            "model-required",
        ]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<Dictation>) {
        let heard = || "Meet me at the café by the market on Thursday at four".to_owned();
        let prompt = match fixture.scenario() {
            Some("listening") => Prompt {
                id: 2,
                stage: Stage::Listening,
                draft: heard(),
                ..Prompt::default()
            },
            Some("preparing") => Prompt {
                id: 2,
                stage: Stage::Preparing,
                ..Prompt::default()
            },
            Some("loading") => Prompt {
                id: 2,
                stage: Stage::Loading,
                ..Prompt::default()
            },
            Some("stopping-microphone") => Prompt {
                id: 2,
                stage: Stage::StoppingMicrophone,
                ..Prompt::default()
            },
            Some("finishing") => Prompt {
                id: 2,
                stage: Stage::Finishing,
                ..Prompt::default()
            },
            Some("recovery-too-long" | "recovery-copied") => Prompt {
                id: 2,
                stage: Stage::Recovery,
                recovery: Some(RecoveryReason::TooLong),
                copied: fixture.scenario() == Some("recovery-copied"),
                ..Prompt::default()
            },
            Some("recovery-vocabulary") => Prompt {
                id: 2,
                stage: Stage::Recovery,
                recovery: Some(RecoveryReason::VocabularyChanged),
                ..Prompt::default()
            },
            Some("recovery-copy-failed") => Prompt {
                id: 2,
                stage: Stage::Recovery,
                recovery: Some(RecoveryReason::TooLong),
                copy_failed: true,
                ..Prompt::default()
            },
            Some("model-required") => Prompt {
                id: 1,
                stage: Stage::Unavailable,
                failure: Some(Failure::ModelRequired),
                ..Prompt::default()
            },
            Some("unavailable") => Prompt {
                id: 1,
                stage: Stage::Unavailable,
                failure: Some(Failure::MicrophoneUnavailable),
                ..Prompt::default()
            },
            _ => Prompt {
                id: 1,
                stage: Stage::Asking,
                ..Prompt::default()
            },
        };
        fixture.provide(prompt);
    }
}

/// What a stage says: its title, the body under it, and a caption below.
struct Words {
    title: String,
    body: String,
    caption: Option<String>,
}

/// The words of the stage shown, or nothing when there is nothing to show.
fn words(shown: &Shown) -> Option<Words> {
    let prompt = shown.prompt.as_ref()?;
    let mut caption = None;
    let (title, body) = match prompt.stage {
        Stage::Asking => {
            caption = Some(t("dictation-ask-caption"));
            (t("dictation-ask-title"), t("dictation-ask-body"))
        }
        Stage::Preparing => (t("dictation-title"), t("dictation-preparing")),
        Stage::Loading => (t("dictation-title"), t("dictation-loading")),
        Stage::StoppingMicrophone => (t("dictation-title"), t("dictation-stopping-microphone")),
        Stage::Stopping => (t("dictation-title"), t("dictation-stopping")),
        Stage::Listening => {
            let body = if prompt.draft.trim().is_empty() {
                t("dictation-listening")
            } else {
                words_with("dictation-live-preview", &[("text", prompt.draft.as_str())])
            };
            (t("dictation-title"), body)
        }
        Stage::Finishing => (t("dictation-title"), t("dictation-finishing")),
        Stage::Recovery => {
            caption = Some(if prompt.copy_failed {
                t("dictation-copy-failed")
            } else if prompt.copied {
                t("dictation-copied")
            } else {
                t("dictation-recovery-kept")
            });
            let body = match prompt.recovery {
                Some(RecoveryReason::VocabularyChanged) => t("dictation-recovery-vocabulary"),
                Some(RecoveryReason::TooLong) | None => t("dictation-recovery-too-long"),
            };
            (t("dictation-recovery-title"), body)
        }
        Stage::Unavailable => {
            let said = prompt.failure.unwrap_or(Failure::Unavailable).key();
            (t("dictation-unavailable-title"), t(said))
        }
    };
    Some(Words {
        title,
        body,
        caption,
    })
}

/// What Escape answers in the stage shown: no to the microphone, keep
/// nothing of what was heard, and shut a card that could do nothing.
fn escape(prompt: &Prompt) -> Answer {
    match prompt.stage {
        Stage::Asking => Answer::Deny(prompt.id),
        Stage::Recovery => Answer::Discard(prompt.id),
        Stage::Preparing
        | Stage::Loading
        | Stage::Listening
        | Stage::StoppingMicrophone
        | Stage::Finishing
        | Stage::Stopping
        | Stage::Unavailable => Answer::Cancel(prompt.id),
    }
}

/// The card the stage draws: the words, and the answers it allows.
fn card(shown: &Shown, seat: Option<&DictationSeat>) -> Element {
    let Some(prompt) = shown.prompt.as_ref() else {
        // Nothing to show: the session has taken the dictation away.
        return Box::new(Column::new());
    };
    let Some(words) = words(shown) else {
        return Box::new(Column::new());
    };
    let id = prompt.id;
    // These buttons answer the seat with a plain local handler
    // (`Button::on_click`), never through `cx.effects(..)` or `cx.actions()`,
    // and the sheet publishes no vocabulary for their answers: an effect is
    // reachable through the session's action door, and a remote action is not
    // consent to listen through the microphone. The session can read what the
    // person answered ([`DictationSeat::take_answers`]); nothing but a press
    // on this card can answer for them.
    let answering = |answer: Answer, button: Button| match seat {
        Some(seat) => {
            let seat = seat.clone();
            button.on_click(move || seat.answer(answer))
        }
        None => button,
    };
    let primary = |id: &str, label: &str| Button::new(id, label).role(ButtonRole::Primary);
    // The primary answer comes first in every stage: it is what the keyboard
    // starts on, so Enter gives it.
    let footer: Element = match prompt.stage {
        Stage::Asking => {
            let allow = answering(
                Answer::Allow(id),
                primary("dictation-allow", &t("dictation-allow")),
            );
            let deny = answering(
                Answer::Deny(id),
                Button::new("dictation-deny", &t("dictation-deny")),
            );
            Box::new(
                Row::new()
                    .with_gap(ui::tokens::Spacing::BASE.s2)
                    .push(Box::new(allow))
                    .push(Box::new(deny)),
            )
        }
        // What is warming up or finishing can be given up; what is already
        // stopping cannot, and closes itself.
        Stage::Preparing | Stage::Loading | Stage::Finishing => Box::new(answering(
            Answer::Cancel(id),
            Button::new("dictation-cancel", &t("dictation-cancel")),
        )),
        Stage::StoppingMicrophone | Stage::Stopping => Box::new(Column::new()),
        Stage::Listening => {
            let finish = answering(
                Answer::Finish(id),
                primary("dictation-finish", &t("dictation-finish")),
            );
            let cancel = answering(
                Answer::Cancel(id),
                Button::new("dictation-cancel", &t("dictation-cancel")),
            );
            Box::new(
                Row::new()
                    .with_gap(ui::tokens::Spacing::BASE.s2)
                    .push(Box::new(finish))
                    .push(Box::new(cancel)),
            )
        }
        Stage::Recovery => {
            let copy = answering(
                Answer::Copy(id),
                primary("dictation-copy", &t("dictation-copy")),
            );
            let discard = answering(
                Answer::Discard(id),
                Button::new("dictation-discard", &t("dictation-discard")),
            );
            Box::new(
                Row::new()
                    .with_gap(ui::tokens::Spacing::BASE.s2)
                    .push(Box::new(copy))
                    .push(Box::new(discard)),
            )
        }
        Stage::Unavailable => Box::new(answering(
            Answer::Cancel(id),
            primary("dictation-close", &t("dictation-close")),
        )),
    };
    let mut body = Column::new()
        .with_rhythm(ui::tokens::Rhythm::Tight)
        .push(Box::new(Text::new(&words.body)));
    if let Some(caption) = &words.caption {
        body = body.push(Box::new(Text::new(caption).with_role(TextRole::Caption)));
    }
    let panel = FloatingPanel::new(Box::new(body))
        .header(Box::new(
            Text::new(&words.title).with_role(TextRole::Subheading),
        ))
        .footer(footer);
    centred(Box::new(panel))
}

/// The card in the middle of the sheet's surface, no wider than one sheet.
fn centred(card: Element) -> Element {
    Box::new(
        Manual::new(|layout| {
            let available = layout.available();
            let area = Rect::new(
                0.0,
                0.0,
                f64::from(available.width),
                f64::from(available.height),
            );
            let metrics = ui::tokens::Metrics::BASE;
            let edge = f64::from(ui::tokens::Spacing::BASE.s4);
            let widest = (area.width() - edge * 2.0).max(f64::from(metrics.panel_width_utility));
            let width = f64::from(metrics.panel_width_sheet).min(widest);
            let measured = layout.measure(
                0,
                ui::taffy::Size {
                    width: MeasureConstraint::Exact(ui::num::f32_of_f64(width)),
                    height: MeasureConstraint::Intrinsic,
                },
            )?;
            let card =
                Rect::from_center_size(area.center(), (width, f64::from(measured.size.height)));
            layout.place(0, card)?;
            Ok(available)
        })
        .child(card),
    )
}

/// The card as a dialog: it traps focus, which starts on the primary answer
/// (so Enter allows the microphone, inserts, copies, or closes, as the card
/// says), Tab walks the answers, and Escape gives the stage's own answer.
struct Answering {
    content: Element,
    shown: LocalState<Shown>,
    seat: Option<DictationSeat>,
}

impl ui::widget::Decorator for Answering {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        use ui::winit::keyboard::{Key, NamedKey};
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ui::winit::event::ElementState::Pressed
            && key.logical_key == Key::Named(NamedKey::Escape)
        {
            if let (Some(seat), Some(prompt)) = (&self.seat, self.shown.get().prompt) {
                seat.answer(escape(&prompt));
            }
            *ctx.handled = true;
            return;
        }
        self.content.handle_event(event, ctx);
    }

    fn traps_focus(&self) -> bool {
        true
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let words = words(&self.shown.get())?;
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(words.title)
                .with_description(words.body)
                .with_state(AccessState::new().modal(true)),
        )
    }
}

/// The app's definition, which the session mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(DictationApp)
}
