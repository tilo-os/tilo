//! Tabs belong to the desktop, never to an application. A Tilo app shows one thing: a
//! page, a file, a conversation. Requests are queued on the UI thread and the app runner
//! hands them to the session shell over the app's authenticated `shell-ui` connection,
//! which knows the asking app and its tab.

use std::cell::RefCell;
use std::collections::VecDeque;

/// What an app may ask of the desktop's tab model about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabRequest {
    OpenBeside {
        target: Option<String>,
        background: bool,
    },
    CloseThis,
    ReopenClosed,
    CycleTab {
        forward: bool,
    },
    PlaceNextWindowBeside {
        background: bool,
    },
    OpenFloated {
        anchor: String,
    },
}

impl TabRequest {
    /// The `shell-ui` message that carries this request for the app the shell launched as
    /// record `instance`.
    #[must_use]
    pub fn message(
        self,
        instance: Option<u64>,
    ) -> crate::floating_surface::FloatingSurfaceClientMessage {
        use crate::floating_surface::FloatingSurfaceClientMessage as Message;
        match self {
            Self::OpenBeside { target, background } => Message::OpenTabBeside {
                target,
                background,
                instance,
            },
            Self::CloseThis => Message::CloseTab { instance },
            Self::ReopenClosed => Message::ReopenClosedTab,
            Self::CycleTab { forward } => Message::CycleTab { forward, instance },
            Self::PlaceNextWindowBeside { background } => Message::PlaceNextWindowBeside {
                background,
                instance,
            },
            Self::OpenFloated { anchor } => Message::OpenFloated { anchor, instance },
        }
    }
}

/// Upper bound on undelivered requests. A held key repeating faster than frames are drawn
/// must not queue unbounded launches.
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

/// Ask the desktop for another instance of this app in a new tab right after this one,
/// opening `target` (a URI) when given.
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

/// Ask the desk for another window of this app, floating where this window's send-to box
/// last sent from, showing `anchor`: the item that send landed in.
pub fn open_floated(anchor: String) {
    push(TabRequest::OpenFloated { anchor });
}

/// Every request queued on this thread, oldest first.
pub fn take_requests() -> Vec<TabRequest> {
    PENDING.with(|pending| pending.borrow_mut().drain(..).collect())
}

/// The record instance the shell launched this process for, which is how it finds this
/// app's own tab.
pub fn launch_instance() -> Option<u64> {
    crate::host::environment()
        .identity
        .continuity_instance
        .as_deref()
        .and_then(|value| value.trim().parse().ok())
        .filter(|instance| *instance != 0)
}

/// The desktop tab chord a key press names, if any: Ctrl+T opens a new tab of this app,
/// Ctrl+Shift+T reopens the last closed tab, Ctrl+W closes this tab, Ctrl+Tab and
/// Ctrl+Shift+Tab show the next and previous tab of the group. `command` is the
/// platform's command modifier (Ctrl, or Cmd on macOS); Alt and Super chords are never
/// these.
#[must_use]
pub fn chord_request(
    key: &winit::keyboard::Key,
    command: bool,
    shift: bool,
    other_modifiers: bool,
) -> Option<TabRequest> {
    if !command || other_modifiers {
        return None;
    }
    let text = match key {
        winit::keyboard::Key::Character(text) => text,
        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab) => {
            return Some(TabRequest::CycleTab { forward: !shift });
        }
        _ => return None,
    };
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
        let tab = Key::Named(NamedKey::Tab);
        assert_eq!(
            chord_request(&tab, true, false, false),
            Some(TabRequest::CycleTab { forward: true }),
            "Ctrl+Tab the app left alone shows the next tab"
        );
        assert_eq!(
            chord_request(&tab, true, true, false),
            Some(TabRequest::CycleTab { forward: false })
        );
        assert_eq!(chord_request(&tab, false, false, false), None, "plain Tab");
        assert_eq!(
            chord_request(&tab, true, false, true),
            None,
            "Super+Ctrl+Tab is the desktop's own, never the app's"
        );
    }

    #[test]
    fn requests_queue_in_order_and_stay_bounded() {
        let _ = take_requests();
        open_beside(Some("https://example.test/".into()), true);
        close_this();
        reopen_closed();
        place_next_window_beside(false);
        open_floated("chat.channels:1".into());
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
                TabRequest::OpenFloated {
                    anchor: "chat.channels:1".into()
                },
            ]
        );
        for _ in 0..(MAX_PENDING * 2) {
            open_beside(None, false);
        }
        assert_eq!(take_requests().len(), MAX_PENDING);
        assert!(take_requests().is_empty());
    }
}
