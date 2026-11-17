//! What the shell says when something a person asked for did not happen: what they asked for, why it
//! did not happen, and — where trying again can work — the way to try.
//! One [`Outcome`] is the whole vocabulary. It is drawn where the thing they touched was (the tab
//! of an app that did not open, the row of a search that could not run), or, when nothing of it is
//! left on the shell, told as a notice. Nothing else words a failure of the person's own action.

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
    /// The technical side of it, for whoever looks into it: what the launch or a service answered.
    /// Never among the words the person is told; its card keeps it behind "Details". Empty where
    /// there is none.
    pub technical_details: String,
}

impl Outcome {
    /// An outcome that leads with `what` .
    #[must_use]
    pub fn new(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            why: String::new(),
            retry: None,
            technical_details: String::new(),
        }
    }

    /// The same outcome, keeping `technical_details` behind its card's "Details".
    #[must_use]
    pub fn with_technical_details(mut self, technical_details: impl Into<String>) -> Self {
        self.technical_details = technical_details.into();
        self
    }

    /// The outcome for the app `app` whose window could not be built: it could not open, what to
    /// do about it, and Try again. `technical_details` is what the launch answered.
    #[must_use]
    pub fn not_built(app: &str, technical_details: impl Into<String>) -> Self {
        Self::new(crate::i18n::t_args(
            "answer-app-not-opened",
            &[("app", app.into())],
        ))
        .saying(crate::i18n::t_args(
            "answer-why-not-built",
            &[("app", app.into())],
        ))
        .retryable(crate::i18n::t("answer-try-again"))
        .with_technical_details(technical_details)
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
    /// the shell to be drawn on: it says what did not happen and why in the notification centre,
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
        // An outcome with a technical reason is the toolkit's own card for something that did not
        // open, which keeps that reason behind "Details".
        if !outcome.technical_details.trim().is_empty() {
            let mut card =
                ui::widget::EmptyState::new(&outcome.what).with_details(&outcome.technical_details);
            if !outcome.why.trim().is_empty() {
                card = card.with_message(&outcome.why);
            }
            if let Some(label) = &outcome.retry {
                card = card
                    .with_action(label)
                    .with_action_id(RETRY_ID)
                    .on_effect(retry);
            }
            return Self {
                title: outcome.what.clone(),
                inner: Box::new(card),
            };
        }
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

    /// A window whose app could not be built says so in the person's words, says what to do, tries
    /// again on their word, and keeps what the launch answered out of what it tells them.
    #[test]
    fn a_window_that_could_not_be_built_says_what_to_do_and_keeps_the_reason_apart() {
        let reason = "application setup failed: application binding does not match";
        let asked = Rc::new(Cell::new(0));
        let counted = Rc::clone(&asked);
        let outcome = Outcome::not_built("Notes", reason);
        assert_eq!(outcome.what, "Notes could not open");
        assert_eq!(
            outcome.why,
            "It ran into a problem while opening. Try again. If it still does not open, close this window and open Notes again."
        );
        assert_eq!(outcome.retry.as_deref(), Some("Try again"));
        assert_eq!(outcome.technical_details, reason);
        assert!(
            !outcome.detail().contains("binding"),
            "the line a notice says holds no technical reason: {}",
            outcome.detail()
        );
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
        // What to do is what the card is described by. What the launch answered is none of the
        // words the window shows: its Details control keeps it.
        let described = descriptions_under(frame.root_widget());
        assert!(
            described.iter().any(|said| said == &outcome.why),
            "{described:?}"
        );
        assert!(
            !texts.iter().any(|shown| shown.contains("binding")),
            "{texts:?}"
        );
        assert!(texts.iter().any(|shown| shown == "Details"), "{texts:?}");
        assert!(described.iter().any(|kept| kept == reason), "{described:?}");
        frame
            .perform_named(RETRY_ID, &ui::a11y::Action::Activate)
            .unwrap();
        assert_eq!(asked.get(), 1, "Try again asked for it once");
    }

    /// The middle of the control named `name` under `root`, where it is drawn.
    fn centre_of_control(root: &dyn Widget, name: &str) -> Option<(f64, f64)> {
        fn centre_in(access: &ui::a11y::AccessNode, name: &str) -> Option<(f64, f64)> {
            if access.name.as_deref() == Some(name)
                && let Some(bounds) = access.bounds
            {
                return Some((
                    f64::midpoint(bounds.x0, bounds.x1),
                    f64::midpoint(bounds.y0, bounds.y1),
                ));
            }
            access
                .children
                .iter()
                .find_map(|child| centre_in(child, name))
        }
        fn centre_under(widget: &dyn Widget, name: &str, found: &mut Option<(f64, f64)>) {
            if found.is_none() {
                *found = widget
                    .accessibility()
                    .and_then(|access| centre_in(&access, name));
            }
            widget.for_each_child(&mut |child| centre_under(child, name, found));
        }
        let mut found = None;
        centre_under(root, name, &mut found);
        found
    }

    /// Every description `root` and its descendants give assistive technology, depth first.
    fn descriptions_under(root: &dyn Widget) -> Vec<String> {
        fn described_in(access: &ui::a11y::AccessNode, out: &mut Vec<String>) {
            out.extend(access.description.clone());
            for child in &access.children {
                described_in(child, out);
            }
        }
        fn described_under(widget: &dyn Widget, out: &mut Vec<String>) {
            if let Some(access) = widget.accessibility() {
                described_in(&access, out);
            }
            widget.for_each_child(&mut |child| described_under(child, out));
        }
        let mut out = Vec::new();
        described_under(root, &mut out);
        out
    }

    /// `TILO_OUTCOME_SHOTS=<dir>` writes the card of a window whose app could not be built, as it
    /// stands and with its Details open, in the dark and light themes.
    #[test]
    fn shots_of_a_window_that_could_not_be_built() {
        let Ok(directory) = std::env::var("TILO_OUTCOME_SHOTS") else {
            return;
        };
        window_not_built_shots(std::path::Path::new(&directory));
    }

    /// The pictures [`shots_of_a_window_that_could_not_be_built`] writes into `directory`.
    fn window_not_built_shots(directory: &std::path::Path) {
        for (scheme, theme) in [("dark", ui::Theme::dark()), ("light", ui::Theme::light())] {
            let outcome = Outcome::not_built(
                "Notes",
                "application setup failed: application binding does not match",
            );
            let card = OutcomeCard::new(&outcome, || {});
            let mut frame = ui::headless::FrameHarness::new(
                ui::state::State::new(),
                Box::new(card),
                (720, 480),
            );
            frame.set_theme(theme);
            frame.relayout();
            frame.render_png(&directory.join(format!("window-not-built-{scheme}.png")));
            // The same card with its Details open, as a press on the control opens it.
            if let Some((x, y)) = centre_of_control(frame.root_widget(), "Details") {
                frame.click_at(x, y);
                frame.relayout();
                frame.render_png(&directory.join(format!("window-not-built-details-{scheme}.png")));
            }
        }
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

    tilo_test_env::scenario! {
        fn an_outcome_says_what_happened_in_a_card_a_line_or_a_notice;

        phase an_outcome_reads_as_one_line;
        // One line says what happened and why, without doubling a full stop.
        {
            assert_eq!(
                Outcome::new("Notes could not open")
                    .saying("it is not installed here")
                    .detail(),
                "Notes could not open: it is not installed here"
            );
            assert_eq!(Outcome::new("No permission.").detail(), "No permission");
        }

        phase an_outcome_with_nothing_to_be_drawn_on_is_a_notice;
        // What has nothing of it left on the shell is told as a notice: its sentence leads and its why
        // follows, waiting in the notification centre.
        {
            let notice = Outcome::new("Screenshots run in Tilo on a computer")
                .saying("A browser page has no screen of its own.")
                .notice();
            assert_eq!(notice.title, "Screenshots run in Tilo on a computer");
            assert_eq!(notice.body, "A browser page has no screen of its own.");
        }
    }
}
