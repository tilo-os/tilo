//! The fixed chords the page's desk leaves to its host: the capture studio
//! and cutting a remote control. Each runs a session surface over a system's
//! screen, and a page has none, so the chord says so through the one
//! [`Outcome`] path rather than doing nothing.
//!
//! The rest of the fixed chords — closing the window in front, a new window,
//! another window of the app in front, the terminal, the strip switcher and
//! the workspace history — never reach here: the desk carries them out on its
//! own tree, as the native desktop does.

use tilo_desktop_surface::i18n::t;
use tilo_desktop_surface::outcome::Outcome;
use ui::shortcut_table::FixedAction;

/// What the page says about the fixed `action` its desk left to the host, or
/// `None` for one the desk answers itself, or one no window is here for.
pub(crate) fn unserved(action: FixedAction) -> Option<Outcome> {
    match action {
        FixedAction::CaptureScreenshot
        | FixedAction::PictureInPicture
        | FixedAction::SaveRecentReplay => Some(
            Outcome::new(t("answer-capture-not-in-browser"))
                .saying(t("answer-why-capture-not-in-browser")),
        ),
        FixedAction::StopRemoteInput => Some(
            Outcome::new(t("answer-remote-not-in-browser"))
                .saying(t("answer-why-remote-not-in-browser")),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chord the page cannot serve says what it is and why, in a notice the
    /// person can read, instead of doing nothing.
    #[test]
    fn a_chord_the_page_cannot_serve_says_so() {
        for action in [
            FixedAction::CaptureScreenshot,
            FixedAction::PictureInPicture,
            FixedAction::SaveRecentReplay,
            FixedAction::StopRemoteInput,
        ] {
            let outcome = unserved(action).unwrap_or_else(|| panic!("{action:?} is answered"));
            let notice = outcome.notice();
            assert!(!notice.title.trim().is_empty(), "{action:?} says what");
            assert!(!notice.body.trim().is_empty(), "{action:?} says why");
        }
    }
}
