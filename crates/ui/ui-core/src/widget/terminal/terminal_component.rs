//! [`Terminal`] as a described component: a shell's grid inside a view.
//!
//! The session a terminal shows belongs to the window, not to the app: the window that draws the
//! view holds the sessions its shell runs and hands them to the `ui.Terminal` nodes it builds, so
//! what crosses from the app is the id of its session. Keys, pastes and pointer reports go back to
//! that session — the toolkit never turns a key into bytes of its own — and what the app records
//! or shows arrives as this component's events.
use std::sync::Arc;

use crate::component::{Component, Described, Event};

use super::{Missing, Terminal};

/// A terminal's grid: what a running shell printed and the input that reaches it.
///
/// The window of the app, the grid and the program inside it are one thing: a terminal has no
/// address bar, no search of its own and no chrome — the app's own window bar carries where the
/// session runs, and find over the scrollback is the window's.
#[derive(Component, Default)]
#[component(name = "Terminal", role = "Terminal", label = "label")]
pub struct TerminalProps {
    /// The session this grid shows, as the app's record names it.
    #[component(required, positional)]
    pub session: String,
    /// What the grid shows, for assistive technology: "Terminal, Build".
    pub label: Option<String>,
    /// Finishing a selection also copies it to the clipboard, not only to the primary selection.
    pub copy_on_select: bool,
    /// Flash the grid when the program rings the bell.
    pub bell_flash: bool,
    /// The program set the window's title.
    #[component(event(value = String))]
    pub title: Event,
    /// A run the shell marked ended; carries it as JSON.
    #[component(event(value = String))]
    pub finished: Event,
    /// The shell ended: carries "exited", its status, or why the connection was lost.
    #[component(event(value = String))]
    pub exit: Event,
    /// A selection was finished: carries the text it copied.
    #[component(event(value = String))]
    pub selected: Event,
    /// The bell rang.
    #[component(event(value = String))]
    pub rung: Event,
}

impl Described for Terminal {
    type Props = TerminalProps;

    fn from_props(_key: &str, props: TerminalProps) -> Self {
        let TerminalProps {
            session,
            label,
            copy_on_select,
            bell_flash,
            title,
            finished,
            exit,
            selected,
            rung,
        } = props;
        let handle = crate::terminal::window_session(&session)
            .unwrap_or_else(|| Arc::new(Missing) as crate::terminal::SessionHandle);
        let mut view = Self::new(handle)
            .with_copy_on_select(copy_on_select)
            .with_bell_flash(bell_flash);
        if let Some(keys) = crate::key_input::window_input(&session) {
            view.set_keys(keys);
        }
        view.session_id = session;
        view.update_events(title, finished, exit, selected, rung);
        if let Some(label) = label {
            view = view.with_accessible_name(label);
        }
        view
    }

    /// The session and what the person's preferences ask of the grid follow where the view
    /// stands; the events a host attached are its own from one build to the next.
    fn set_props(&mut self, props: TerminalProps) -> bool {
        let mut changed = false;
        if self.session_id != props.session {
            self.show_session(&props.session);
            changed = true;
        }
        if props.copy_on_select != self.copy_on_select {
            self.copy_on_select = props.copy_on_select;
            changed = true;
        }
        if props.bell_flash != self.bell_flash {
            self.bell_flash = props.bell_flash;
            changed = true;
        }
        if let Some(label) = props.label {
            self.set_accessible_name(label);
        }
        self.update_events(
            props.title,
            props.finished,
            props.exit,
            props.selected,
            props.rung,
        );
        changed
    }
}
