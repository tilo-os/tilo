//! Tabs belong to the desktop, never to an application.
//!
//! A Tilo app shows one thing: a page, a file, a conversation. Several of
//! them side by side are several app instances, and the shell groups them
//! as tabs of a window group (`Workspace → Strip → WindowGroup → Tab`). An
//! app that wants "another one of these next to me" — a link opened in a
//! new tab, Ctrl+T, a file opened beside the current one — asks the shell
//! for it here instead of growing a tab strip of its own.
//!
//! Requests are queued on the UI thread and the app runner hands them to
//! the session shell over the app's authenticated `shell-ui` connection,
//! which knows the asking app and its tab. Outside a Tilo session there is
//! no desktop tab model: the runner opens a new window of the same app
//! instead (another process given the same target), so "beside me" still
//! means "another instance", never a tab inside this window.
//!
//! The keyboard is the same everywhere: Ctrl+T, Ctrl+W and Ctrl+Shift+T
//! (Cmd on macOS) reach these requests when the focused widget leaves the
//! chord unhandled, so every app gets the desktop's tab chords without
//! implementing them.

use std::cell::RefCell;
use std::collections::VecDeque;

/// What an app may ask of the desktop's tab model about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabRequest {
    /// Open another instance of this app as a tab right after this app's
    /// own tab. `target` is what the new instance opens — a URI the app
    /// declares in its manifest — or `None` for a fresh, empty instance.
    /// A `background` tab does not take focus from this one.
    OpenBeside {
        target: Option<String>,
        background: bool,
    },
    /// Close this app's own tab through the desktop's tab lifecycle, so it
    /// can be reopened like any other closed tab.
    CloseThis,
    /// Reopen the most recently closed tab of this workspace.
    ReopenClosed,
    /// The next window this app maps is another tab right after this one.
    /// For a window the app starts itself (a process it spawned), where the
    /// shell launches nothing.
    PlaceNextWindowBeside { background: bool },
}

/// Upper bound on undelivered requests. A held key repeating faster than
/// frames are drawn must not queue unbounded launches.
const MAX_PENDING: usize = 16;

thread_local! {
    static PENDING: RefCell<VecDeque<TabRequest>> = const { RefCell::new(VecDeque::new()) };
}

fn push(request: TabRequest) {
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        if pending.len() < MAX_PENDING {
            pending.push_back(request);
        }
    });
}

/// Ask the desktop for another instance of this app in a new tab right
/// after this one, opening `target` (a URI) when given.
pub fn open_beside(target: Option<String>, background: bool) {
    push(TabRequest::OpenBeside { target, background });
}

/// Close this app's tab the way the desktop closes any tab.
pub fn close_this() {
    push(TabRequest::CloseThis);
}

/// Reopen the workspace's most recently closed tab.
pub fn reopen_closed() {
    push(TabRequest::ReopenClosed);
}

/// Place the next window this app maps as a tab right after this one.
pub fn place_next_window_beside(background: bool) {
    push(TabRequest::PlaceNextWindowBeside { background });
}

/// Every request queued on this thread, oldest first.
pub fn take_requests() -> Vec<TabRequest> {
    PENDING.with(|pending| pending.borrow_mut().drain(..).collect())
}

/// The record instance the shell launched this process for, which is how it
/// finds this app's own tab. `None` outside a shell launch.
pub fn launch_instance() -> Option<u64> {
    crate::host::environment()
        .identity
        .continuity_instance
        .as_deref()
        .and_then(|value| value.trim().parse().ok())
        .filter(|instance| *instance != 0)
}

/// The desktop tab chord a key press names, if any: Ctrl+T opens a new tab
/// of this app, Ctrl+Shift+T reopens the last closed tab, Ctrl+W closes this
/// tab. `command` is the platform's command modifier (Ctrl, or Cmd on
/// macOS); Alt and Super chords are never these.
#[must_use]
pub fn chord_request(
    key: &winit::keyboard::Key,
    command: bool,
    shift: bool,
    other_modifiers: bool,
) -> Option<TabRequest> {
    let winit::keyboard::Key::Character(text) = key else {
        return None;
    };
    if !command || other_modifiers {
        return None;
    }
    if text.eq_ignore_ascii_case("t") {
        Some(if shift {
            TabRequest::ReopenClosed
        } else {
            TabRequest::OpenBeside {
                target: None,
                background: false,
            }
        })
    } else if text.eq_ignore_ascii_case("w") && !shift {
        Some(TabRequest::CloseThis)
    } else {
        None
    }
}

/// Queue the request `chord_request` named.
pub fn request(request: TabRequest) {
    push(request);
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, NamedKey};

    #[test]
    fn chords_name_the_desktop_tab_requests_and_nothing_else() {
        let t = Key::Character("t".into());
        let w = Key::Character("W".into());
        assert_eq!(
            chord_request(&t, true, false, false),
            Some(TabRequest::OpenBeside {
                target: None,
                background: false
            })
        );
        assert_eq!(
            chord_request(&t, true, true, false),
            Some(TabRequest::ReopenClosed)
        );
        assert_eq!(
            chord_request(&w, true, false, false),
            Some(TabRequest::CloseThis)
        );
        assert_eq!(chord_request(&w, true, true, false), None);
        assert_eq!(
            chord_request(&t, false, false, false),
            None,
            "plain t types"
        );
        assert_eq!(
            chord_request(&t, true, false, true),
            None,
            "Alt/Super chords"
        );
        assert_eq!(
            chord_request(&Key::Named(NamedKey::Tab), true, false, false),
            None,
            "Ctrl+Tab is the shell's own binding"
        );
    }

    #[test]
    fn requests_queue_in_order_and_stay_bounded() {
        let _ = take_requests();
        open_beside(Some("https://example.test/".into()), true);
        close_this();
        reopen_closed();
        place_next_window_beside(false);
        assert_eq!(
            take_requests(),
            vec![
                TabRequest::OpenBeside {
                    target: Some("https://example.test/".into()),
                    background: true
                },
                TabRequest::CloseThis,
                TabRequest::ReopenClosed,
                TabRequest::PlaceNextWindowBeside { background: false },
            ]
        );
        for _ in 0..(MAX_PENDING * 2) {
            open_beside(None, false);
        }
        assert_eq!(take_requests().len(), MAX_PENDING);
        assert!(take_requests().is_empty());
    }
}
