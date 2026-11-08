//! What the desk says when something a person asked for did not happen: what they asked for, why it
//! did not happen, and — where trying again can work — the way to try.
//! One [`Outcome`] is the whole vocabulary. It is drawn where the thing they touched was (the tab
//! of an app that did not open, the row of a search that could not run), or, when nothing of it is
//! left on the desk, told as a notice. Nothing else words a failure of the person's own action.

use ui::widget::{
    AppKind, AppWidget, Button, ButtonRole, Column, Container, Element, Text, Widget,
};

/// The button that tries the outcome's action again, for tests and assistive technology.
pub const RETRY_ID: &str = "outcome-retry";

/// Something a person asked for that did not happen, as they are told it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The sentence it leads with, in the person's words: "Notes could not open", "That screenshot
    /// was not kept".
    pub what: String,
    /// Why, in words they can act on: "it is not installed on this computer". Empty where what
    /// happened is all there is to say.
    pub why: String,
    /// The label of the button that tries it again, where trying again can work; `None` tells it
    /// without a button.
    pub retry: Option<String>,
}

impl Outcome {
    /// An outcome that leads with `what` .
    #[must_use]
    pub fn new(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            why: String::new(),
            retry: None,
        }
    }

    /// The same outcome, saying `why` .
    #[must_use]
    pub fn saying(mut self, why: impl Into<String>) -> Self {
        self.why = why.into();
        self
    }

    /// The same outcome, offering to try again under `label` .
    #[must_use]
    pub fn retryable(mut self, label: impl Into<String>) -> Self {
        self.retry = Some(label.into());
        self
    }

    /// The whole outcome in one line: what happened, and why when there is a why.
    #[must_use]
    pub fn detail(&self) -> String {
        let why = self.why.trim();
        if why.is_empty() {
            return self.what.trim_end_matches('.').to_owned();
        }
        if self.what.trim().is_empty() {
            return why.to_owned();
        }
        let mut said = self.what.trim_end_matches('.').to_owned();
        // A reason written as a sentence of its own follows as one; one written as a clause follows
        // the colon.
        let sentence = why.chars().next().is_some_and(char::is_uppercase);
        said.push_str(if sentence { ". " } else { ": " });
        said.push_str(why);
        said
    }

    /// The same outcome as a notice, for what the person asked for that has nothing of it left on
    /// the desk to be drawn on: it says what did not happen and why in the notification centre,
    /// where it waits to be read.
    #[must_use]
    pub fn notice(&self) -> crate::notifications::Notification {
        crate::notifications::Notification::new(self.what.clone(), self.why.clone())
    }
}

/// Why an app a window showed is not running any more.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Closed {
    /// It stopped working.
    #[default]
    Crashed,
    /// The session closed it to free the memory it held.
    Memory,
    /// It kept taking more memory, so the session closed it to keep the rest of the desktop
    /// running.
    Runaway,
}

impl Closed {
    /// The outcome for the app `app` that is not here any more, trying again under `retry` .
    #[must_use]
    pub fn outcome(self, app: &str, retry: impl Into<String>) -> Outcome {
        let app = ("app", app.into());
        let (what, why) = match self {
            Self::Crashed => (
                crate::i18n::t_args("answer-app-stopped", &[app]),
                crate::i18n::t("answer-why-stopped"),
            ),
            Self::Memory => (
                crate::i18n::t_args("answer-app-closed-for-memory", &[app]),
                String::new(),
            ),
            Self::Runaway => (
                crate::i18n::t_args("answer-app-closed-for-runaway", &[app]),
                crate::i18n::t("answer-why-runaway"),
            ),
        };
        Outcome::new(what).saying(why).retryable(retry)
    }
}

/// A window in place of what did not happen: the outcome, and a button that tries it again when it
/// offers one.
pub struct OutcomeCard {
    title: String,
    inner: Element,
}

impl OutcomeCard {
    /// The card for `outcome` ; `retry` runs when the person presses the button its label names.
    pub fn new(outcome: &Outcome, retry: impl FnMut() + 'static) -> Self {
        let mut body = Column::new()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(
                Text::new(&outcome.what).with_role(ui::TextRole::Title),
            ));
        if !outcome.why.trim().is_empty() {
            body = body.push(Box::new(Text::new(&outcome.why).dimmed()));
        }
        if let Some(label) = &outcome.retry {
            body = body.push(Box::new(
                Button::new(RETRY_ID, label)
                    .role(ButtonRole::Primary)
                    .on_effect(retry),
            ));
        }
        Self {
            title: outcome.what.clone(),
            inner: Box::new(Container::new().centered().with_child(Box::new(body))),
        }
    }
}

impl ui::widget::Decorator for OutcomeCard {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.inner.as_ref()
    }
    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.inner.as_mut()
    }
    fn name(&self) -> &'static str {
        "OutcomeCard"
    }
}

impl AppWidget for OutcomeCard {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &str {
        &self.title
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("!")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// The card names what did not happen, says why, and tries it again on the person's word, from
    /// the pointer, the keyboard or a screen reader.
    #[test]
    fn an_outcome_says_what_and_why_and_tries_again() {
        let asked = Rc::new(Cell::new(0));
        let counted = Rc::clone(&asked);
        let outcome = Outcome::new("Notes could not open")
            .saying("it is not installed on this computer")
            .retryable("Try again");
        let card = OutcomeCard::new(&outcome, move || {
            counted.set(counted.get() + 1);
        });
        let mut frame =
            ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(card), (640, 420));
        frame.relayout();
        let texts = ui::a11y::texts(frame.root_widget());
        assert!(
            texts.iter().any(|text| text == "Notes could not open"),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|text| text == "it is not installed on this computer"),
            "{texts:?}"
        );
        frame
            .perform_named(RETRY_ID, &ui::a11y::Action::Activate)
            .unwrap();
        assert_eq!(asked.get(), 1, "Try again asked for it once");
    }

    /// What an outcome that trying again cannot fix says, and that it offers no button.
    #[test]
    fn an_outcome_with_nothing_to_try_offers_no_button() {
        let outcome =
            Outcome::new("That screenshot was not kept").saying("the folder is read-only");
        let card = OutcomeCard::new(&outcome, || {});
        let mut frame =
            ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(card), (640, 420));
        frame.relayout();
        let texts = ui::a11y::texts(frame.root_widget());
        assert!(
            texts
                .iter()
                .any(|text| text == "That screenshot was not kept"),
            "{texts:?}"
        );
        assert!(!texts.iter().any(|text| text == "Try again"), "{texts:?}");
    }

    /// One line says what happened and why, without doubling a full stop.
    #[test]
    fn an_outcome_reads_as_one_line() {
        assert_eq!(
            Outcome::new("Notes could not open")
                .saying("it is not installed here")
                .detail(),
            "Notes could not open: it is not installed here"
        );
        assert_eq!(Outcome::new("No permission.").detail(), "No permission");
    }

    /// What has nothing of it left on the desk is told as a notice: its sentence leads and its why
    /// follows, waiting in the notification centre.
    #[test]
    fn an_outcome_with_nothing_to_be_drawn_on_is_a_notice() {
        let notice = Outcome::new("Screenshots run in Tilo on a computer")
            .saying("A browser page has no screen of its own.")
            .notice();
        assert_eq!(notice.title, "Screenshots run in Tilo on a computer");
        assert_eq!(notice.body, "A browser page has no screen of its own.");
    }
}
