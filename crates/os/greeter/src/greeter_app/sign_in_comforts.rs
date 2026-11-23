//! The card's comforts and the device's sign-in comforts: the screen reader
//! toggle starts where the login screens were left, and what is turned on
//! or off here, by the card's toggles or the screen reader chord, is kept
//! for the next login screen (see [`crate::comforts`]).

use ui::a11y::AccessibilityRequest;

use super::GreeterApp;

impl GreeterApp {
    /// Start with the device's screen reader setting, and keep what is
    /// turned on or off here for the login screens through `changes`.
    #[must_use]
    pub fn with_sign_in_comforts(
        mut self,
        screen_reader: bool,
        changes: crate::comforts::Changes,
    ) -> Self {
        self.screen_reader = screen_reader;
        self.sign_in_comforts = Some(changes);
        self
    }

    /// Show a change the card asked for and keep it for the login screens.
    pub(super) fn keep_accessibility_change(&mut self, request: AccessibilityRequest) {
        if let AccessibilityRequest::ScreenReader(on) = request {
            self.screen_reader = on;
        }
        if let Some(changes) = &self.sign_in_comforts {
            match request {
                AccessibilityRequest::ScreenReader(on) => changes.set("screen_reader", on),
                AccessibilityRequest::HighContrast(on) => changes.set("high_contrast", on),
                AccessibilityRequest::TextScale(scale) => changes.set("text_scale", scale),
            }
        }
        // The toggles show their new state; the App's theme change rebuilds.
        self.dirty_structure = true;
    }

    /// Whether `payload` says the chord turned the reader over. The App
    /// already did; the toggle shows it and the login screens keep it.
    pub(super) fn keep_screen_reader_chord(&mut self, payload: &str) -> bool {
        let Some(on) = crate::comforts::chord_toggle(payload) else {
            return false;
        };
        self.screen_reader = on;
        if let Some(changes) = &self.sign_in_comforts {
            changes.set("screen_reader", on);
        }
        self.dirty_structure = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufRead;

    use automerge::AutoCommit;
    use ui::widget::Widget;

    use super::super::tests::one_session;
    use super::*;

    #[test]
    fn the_chord_shows_on_the_card_and_is_kept_for_the_login_screens() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("socket");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut app = GreeterApp::with_sessions(one_session())
            .with_sign_in_comforts(false, crate::comforts::Changes::to(socket));
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&doc, &root);
        app.on_external_event(
            &crate::comforts::chord_payload(true),
            &mut ui::widget::Edits::lend(&mut doc),
            &root,
        );
        assert!(app.screen_reader, "the toggle shows the reader as on");
        let (stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut request)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&request).unwrap(),
            serde_json::json!({"op": "set-sign-in-comforts", "comforts": {"screen_reader": true}})
        );
    }
}
