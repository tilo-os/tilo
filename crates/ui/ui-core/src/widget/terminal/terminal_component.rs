//! [`Terminal`] as a described component: a shell's grid inside a view.
//!
//! The session a terminal shows belongs to the window, not to the app: the window that draws the
//! view holds the sessions its shell runs and hands them to the `ui.Terminal` nodes it builds, so
//! what crosses from the app is the id of its session. Keys, pastes and pointer reports go back to
//! that session — the toolkit never turns a key into bytes of its own — and what the app records
//! or shows arrives as this component's events.
use std::sync::Arc;

use tilo_ui_base::{component::Component, component::Described, component::Event};

use super::{GridEvents, Missing, Terminal};

/// A terminal's grid: what a running shell printed and the input that reaches it.
///
/// The window of the app, the grid and the program inside it are one thing: a terminal has no
/// address bar, no search of its own and no chrome — the app's own window bar carries where the
/// session runs, and find over the scrollback is the window's.
#[derive(Component, Default)]
#[component(
    name = "Terminal",
    role = "Terminal",
    label = "label",
    not_in_gallery = "a terminal shows a shell session the window's host runs, and the gallery's window runs none"
)]
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
    /// Let the program put text on the clipboard (OSC 52): off unless the person allowed it.
    pub allow_clipboard: bool,
    /// The program set the window's title.
    #[component(event(value = String))]
    pub title: Event,
    /// A run the shell marked ended; carries it as JSON.
    #[component(event(value = String))]
    pub finished: Event,
    /// The shell exited: carries "exited", or its status.
    #[component(event(value = String))]
    pub exit: Event,
    /// The connection to the shell was lost: carries why.
    #[component(event(value = String))]
    pub lost: Event,
    /// A selection was finished: carries the text it copied.
    #[component(event(value = String))]
    pub selected: Event,
    /// The bell rang.
    #[component(event(value = String))]
    pub rung: Event,
    /// The shell asked for the lines its Up recalls: carries the ask as JSON, which the app
    /// answers from the history it keeps.
    #[component(event(value = String))]
    pub history: Event,
    /// The session's host holds the command at the prompt until someone acts: carries why as
    /// JSON, and empty text once it let it go.
    #[component(event(value = String))]
    pub held: Event,
}

impl Described for Terminal {
    type Props = TerminalProps;

    fn from_props(_key: &str, props: TerminalProps) -> Self {
        let TerminalProps {
            session,
            label,
            copy_on_select,
            bell_flash,
            allow_clipboard,
            title,
            finished,
            exit,
            lost,
            selected,
            rung,
            history,
            held,
        } = props;
        let handle = crate::terminal::window_session(&session)
            .unwrap_or_else(|| Arc::new(Missing) as crate::terminal::SessionHandle);
        let mut view = Self::new(handle)
            .with_copy_on_select(copy_on_select)
            .with_bell_flash(bell_flash)
            .with_allow_clipboard(allow_clipboard);
        if let Some(keys) = tilo_ui_base::key_input::window_input(&session) {
            view.set_keys(keys);
        }
        view.session_id = session;
        view.update_events(GridEvents {
            title,
            finished,
            exit,
            lost,
            selected,
            rung,
            history,
            held,
        });
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
        if props.allow_clipboard != self.allow_clipboard {
            self.set_allow_clipboard(props.allow_clipboard);
        }
        if props.bell_flash != self.bell_flash {
            self.bell_flash = props.bell_flash;
            changed = true;
        }
        if let Some(label) = props.label {
            self.set_accessible_name(label);
        }
        self.update_events(GridEvents {
            title: props.title,
            finished: props.finished,
            exit: props.exit,
            lost: props.lost,
            selected: props.selected,
            rung: props.rung,
            history: props.history,
            held: props.held,
        });
        if changed {
            self.dirty = true;
        }
        // The props are taken where the terminal stands, so the person's selection and scroll
        // stay: nothing about the grid is built again.
        true
    }
}
