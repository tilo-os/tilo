//! What a window shows in place of an app that is not running any more: what
//! happened, in plain words, and a button that opens the app in it again.
//!
//! A browser page cannot survive a panic, so the page starts over and the
//! window whose app crashed comes back showing this instead of running the
//! app (which would take the whole desktop down again). Reopen is the
//! person's choice to try once more.

use ui::widget::{
    AppKind, AppWidget, Button, ButtonRole, Column, Container, Element, Text, Widget,
};

/// The Reopen button's id, for tests and assistive technology.
pub const REOPEN_ID: &str = "crashed-app-reopen";

/// Why the app the window shows is not running any more.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Closed {
    /// It stopped working.
    #[default]
    Crashed,
    /// The session closed it to free the memory it held.
    Memory,
    /// It kept taking more memory, so the session closed it to keep the rest
    /// of the desktop running.
    Runaway,
}

impl Closed {
    /// The words that say why the app is not here, naming it.
    #[must_use]
    pub fn detail(self, app: &str) -> String {
        let key = match self {
            Self::Crashed => "browser-app-crashed",
            Self::Memory => "browser-app-closed-for-memory",
            Self::Runaway => "browser-app-closed-for-runaway",
        };
        crate::i18n::t_args(key, &[("app", app.into())])
    }
}

/// A window whose app is not running any more.
pub struct CrashedApp {
    title: String,
    inner: Element,
}

impl CrashedApp {
    /// The notice for the app `title`, closed for `closed`; `reopen` runs
    /// when the person asks for the app again.
    pub fn new(title: &str, closed: Closed, reopen: impl FnMut() + 'static) -> Self {
        let detail = closed.detail(title);
        let body = Column::new()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(Text::new(title).with_role(ui::TextRole::Title)))
            .push(Box::new(Text::new(&detail).dimmed()))
            .push(Box::new(
                Button::new(REOPEN_ID, &crate::i18n::t("browser-app-crashed-reopen"))
                    .role(ButtonRole::Primary)
                    .on_effect(reopen),
            ));
        Self {
            title: title.to_owned(),
            inner: Box::new(Container::new().centered().with_child(Box::new(body))),
        }
    }
}

impl ui::widget::Decorator for CrashedApp {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.inner.as_ref()
    }
    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.inner.as_mut()
    }
    fn name(&self) -> &'static str {
        "CrashedApp"
    }
}

impl AppWidget for CrashedApp {
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

    /// The notice names the app, says what happened and reopens it on the
    /// person's word, from the pointer, the keyboard or a screen reader.
    #[test]
    fn reopen_asks_for_the_app_again() {
        let asked = Rc::new(Cell::new(0));
        let counted = Rc::clone(&asked);
        let notice = CrashedApp::new("Canvas", Closed::Crashed, move || {
            counted.set(counted.get() + 1);
        });
        let mut frame =
            ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(notice), (640, 420));
        frame.relayout();
        let texts = ui::a11y::texts(frame.root_widget());
        assert!(
            texts
                .iter()
                .any(|text| text.starts_with("Canvas stopped working")),
            "{texts:?}"
        );
        assert!(texts.iter().any(|text| text == "Reopen"), "{texts:?}");
        frame
            .perform_named(REOPEN_ID, &ui::a11y::Action::Activate)
            .unwrap();
        assert_eq!(asked.get(), 1, "Reopen asked for the app once");
    }

    /// An app the session closed to free memory says that, not that it
    /// stopped working, and still offers to open it again.
    #[test]
    fn an_app_closed_for_memory_says_why() {
        for (closed, said) in [
            (Closed::Memory, "closed to free memory"),
            (Closed::Runaway, "was using too much memory"),
        ] {
            let notice = CrashedApp::new("Canvas", closed, || {});
            let mut frame = ui::headless::FrameHarness::new(
                ui::state::State::new(),
                Box::new(notice),
                (640, 420),
            );
            frame.relayout();
            let texts = ui::a11y::texts(frame.root_widget());
            assert!(
                texts.iter().any(|text| text.contains(said)),
                "{closed:?}: {texts:?}"
            );
            assert!(
                !texts.iter().any(|text| text.contains("stopped working")),
                "the session did not crash it: {texts:?}"
            );
            assert!(texts.iter().any(|text| text == "Reopen"), "{texts:?}");
        }
    }
}
