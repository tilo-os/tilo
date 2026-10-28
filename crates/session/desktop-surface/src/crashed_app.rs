//! What a window shows in place of an app that crashed: what happened, in
//! plain words, and a button that opens the app in it again.
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

/// A window whose app crashed an earlier load of the page.
pub struct CrashedApp {
    title: String,
    inner: Element,
}

impl CrashedApp {
    /// The notice for the app `title`; `reopen` runs when the person asks
    /// for the app again.
    pub fn new(title: &str, reopen: impl FnMut() + 'static) -> Self {
        let detail = crate::i18n::t_args("browser-app-crashed", &[("app", title.into())]);
        let body = Column::new()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(Box::new(Text::new(title).with_role(ui::TextRole::Title)))
            .push(Box::new(Text::new(&detail).dimmed()))
            .push(Box::new(
                Button::new(REOPEN_ID, &crate::i18n::t("browser-app-crashed-reopen"))
                    .role(ButtonRole::Primary)
                    .on_click(reopen),
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
    // The notice stands in for the app: the window's record stays the app's.
    fn uses_host_record(&self) -> bool {
        false
    }
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
        let notice = CrashedApp::new("Canvas", move || counted.set(counted.get() + 1));
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
}
