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
/// `None` for one the desk answers itself.
pub(crate) fn unserved(action: FixedAction) -> Option<Outcome> {
    Some(match action {
        FixedAction::CaptureScreenshot
        | FixedAction::PictureInPicture
        | FixedAction::SaveRecentReplay => Outcome::new(t("answer-capture-not-in-browser"))
            .saying(t("answer-why-capture-not-in-browser")),
        FixedAction::StopRemoteInput => Outcome::new(t("answer-remote-not-in-browser"))
            .saying(t("answer-why-remote-not-in-browser")),
        // A window's chord with no window to act on: the native session says
        // nothing there either, so there is nothing a person asked for.
        FixedAction::ShortcutGuide
        | FixedAction::CloseTab
        | FixedAction::NewWindow
        | FixedAction::DuplicateTab
        | FixedAction::OpenTerminal
        | FixedAction::StripSwitcher
        | FixedAction::WorkspaceHistory
        | FixedAction::ExploreWorkspaceHistory => return None,
    })
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
            assert!(!outcome.what.trim().is_empty(), "{action:?} says what");
            assert!(!outcome.why.trim().is_empty(), "{action:?} says why");
            let notice = outcome.notice();
            assert_eq!(notice.title, outcome.what);
            assert_eq!(notice.body, outcome.why);
        }
    }

    /// The chords the desk carries out on its own tree leave the page nothing
    /// to say: they answer before the host is asked.
    #[test]
    fn a_chord_the_desk_carries_out_is_not_the_pages() {
        for action in [
            FixedAction::CloseTab,
            FixedAction::NewWindow,
            FixedAction::DuplicateTab,
            FixedAction::OpenTerminal,
            FixedAction::StripSwitcher,
            FixedAction::WorkspaceHistory,
            FixedAction::ExploreWorkspaceHistory,
        ] {
            assert!(unserved(action).is_none(), "{action:?} is the desk's");
        }
    }
}
