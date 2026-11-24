//! The toolkit's view of a live session, from the engine's side.
//!
//! The grid, the pty and the key table are this crate's; the toolkit's `ui.Terminal` widget
//! draws a grid and hands keys back through the traits it defines, and knows nothing about what
//! runs the shell. This is the one translation between the two, compiled here so the toolkit
//! links no engine: it installs itself as the process's [`ui::terminal::Engine`] when a session
//! is first kept ([`hand_to_toolkit`]), and every window the process draws then shows that
//! session's grid.

use std::sync::{Arc, Mutex};

use ui::key_input::{Button, Held, Key, Keys, Named};
use ui::terminal::{
    CellColor, GridPos, Line, Modes, Placement, Screen, Session, SessionEvent, SessionHandle, Style,
};

use crate::live::{Event, Live, attached, generation};

/// One live session as the toolkit reads it.
pub struct LiveSession {
    live: Arc<Live>,
    /// The grid converted for the frame being drawn, and the revision it was converted from.
    shown: Mutex<Option<(u64, Arc<Screen>)>>,
}

impl LiveSession {
    #[must_use]
    pub fn new(live: Arc<Live>) -> Self {
        // What the window drawing this grid wakes when the shell prints: the handle is this
        // frame's, so output reaches the window without waiting for the next input.
        let wake = ui::WakeHandle::new();
        if wake.is_active() && !wake.is_detached() {
            live.set_wake(Arc::new(move || wake.wake()));
        }
        Self {
            live,
            shown: Mutex::new(None),
        }
    }

    /// The live session behind this handle.
    #[must_use]
    pub fn live(&self) -> &Arc<Live> {
        &self.live
    }

    fn converted(&self, revision: u64) -> Arc<Screen> {
        let mut shown = self
            .shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((shown_revision, screen)) = shown.as_ref()
            && *shown_revision == revision
        {
            return Arc::clone(screen);
        }
        let engine = self.live.screen();
        let screen = Arc::new(Screen {
            lines: engine
                .active()
                .iter()
                .map(|line| Line {
                    text: line.text.clone(),
                    styles: line
                        .attrs
                        .iter()
                        .map(|attr| Style {
                            fg: cell_color(attr.fg),
                            bg: cell_color(attr.bg),
                            bold: attr.bold,
                            italic: attr.italic,
                            underline: attr.underline,
                            reverse: attr.reverse,
                            dim: attr.dim,
                            strike: attr.strike,
                            wide_cont: attr.is_wide_cont,
                            link: attr.link_id,
                        })
                        .collect(),
                })
                .collect(),
            alternate: engine.alternate,
            cursor: (
                usize::try_from(engine.cursor.0).unwrap_or(0),
                usize::try_from(engine.cursor.1).unwrap_or(0),
            ),
            links: engine.links.clone(),
        });
        *shown = Some((revision, Arc::clone(&screen)));
        screen
    }
}

fn cell_color(color: crate::CellColor) -> CellColor {
    use crate::CellColor as Engine;
    match color {
        Engine::Default => CellColor::Default,
        Engine::Indexed(index) => CellColor::Indexed(index),
        Engine::Rgb(r, g, b) => CellColor::Rgb(r, g, b),
    }
}

fn modes(modes: crate::TermModes) -> Modes {
    Modes {
        cursor_visible: modes.cursor_visible,
        bracketed_paste: modes.bracketed_paste,
        app_cursor_keys: modes.app_cursor_keys,
        mouse_report: modes.mouse_report,
        mouse_sgr: modes.mouse_sgr,
        focus_report: modes.focus_report,
    }
}

fn engine_key(key: Key) -> crate::input::Key {
    use crate::input as engine;
    match key {
        Key::Char(character) => engine::Key::Char(character),
        Key::Named(named) => engine::Key::Named(match named {
            Named::Enter => engine::Named::Enter,
            Named::Space => engine::Named::Space,
            Named::Backspace => engine::Named::Backspace,
            Named::Tab => engine::Named::Tab,
            Named::Escape => engine::Named::Escape,
            Named::ArrowUp => engine::Named::ArrowUp,
            Named::ArrowDown => engine::Named::ArrowDown,
            Named::ArrowLeft => engine::Named::ArrowLeft,
            Named::ArrowRight => engine::Named::ArrowRight,
            Named::Home => engine::Named::Home,
            Named::End => engine::Named::End,
            Named::PageUp => engine::Named::PageUp,
            Named::PageDown => engine::Named::PageDown,
            Named::Insert => engine::Named::Insert,
            Named::Delete => engine::Named::Delete,
            Named::Function(number) => engine::Named::Function(number),
        }),
    }
}

fn held(held: Held) -> crate::input::Held {
    crate::input::Held::new(held.shift, held.alt, held.ctrl, held.sup)
}

fn button(button: Button) -> crate::input::Button {
    match button {
        Button::Left => crate::input::Button::Left,
        Button::Middle => crate::input::Button::Middle,
        Button::Right => crate::input::Button::Right,
    }
}

impl Keys for LiveSession {
    fn key(&self, key: Key, text: Option<&str>, held_beside: Held) {
        let modes = self.live.modes();
        if let Some(bytes) =
            crate::input::encode_key(engine_key(key), text, held(held_beside), modes)
        {
            let _ = self.live.write(&bytes);
        }
    }

    fn text(&self, text: &str) {
        let _ = self.live.write(text.as_bytes());
    }

    fn paste(&self, text: &str) {
        let bytes = crate::input::encode_paste(text, self.live.modes());
        let _ = self.live.write(&bytes);
    }

    fn mouse(
        &self,
        pressed_button: Button,
        pressed: bool,
        held_beside: Held,
        column: usize,
        row: usize,
    ) -> bool {
        let modes = self.live.modes();
        let Some(bytes) = crate::input::encode_mouse(
            modes,
            button(pressed_button),
            pressed,
            held(held_beside),
            column,
            row,
        ) else {
            return false;
        };
        let _ = self.live.write(&bytes);
        true
    }

    fn focus(&self, focused: bool) {
        self.live.focus(focused);
    }
}

impl Session for LiveSession {
    fn screen(&self) -> Arc<Screen> {
        self.converted(self.live.revision())
    }

    fn modes(&self) -> Modes {
        modes(self.live.modes())
    }

    fn revision(&self) -> u64 {
        self.live.revision()
    }

    fn resize(&self, columns: u16, rows: u16) {
        let _ = self.live.resize(columns, rows);
    }

    fn selection_text(&self, anchor: GridPos, cursor: GridPos) -> Option<String> {
        self.live.selection_text(
            crate::GridPos::new(anchor.row, anchor.col),
            crate::GridPos::new(cursor.row, cursor.col),
        )
    }

    fn placements(&self) -> Vec<Placement> {
        self.live
            .placements()
            .into_iter()
            .map(|placement| Placement {
                hash: placement.hash,
                mime: placement.mime,
                row: placement.row,
                column: placement.col,
                columns: placement.columns,
                rows: placement.rows,
                alternate: placement.alternate_screen,
            })
            .collect()
    }

    fn image_bytes(&self, hash: &str) -> Option<Vec<u8>> {
        self.live.image(hash)
    }

    fn public_link(&self, url: &str) -> bool {
        crate::is_public_web_link(url)
    }

    fn events(&self, since: u64) -> (Vec<SessionEvent>, u64) {
        let (events, next) = self.live.events(since);
        let events = events
            .into_iter()
            .map(|event| match event {
                Event::Title(title) => SessionEvent::Title(title),
                Event::Finished(run) => SessionEvent::Finished(finished_json(&run)),
                Event::Exit(code) => SessionEvent::Exit(code),
                Event::Lost(why) => SessionEvent::Lost(why),
                Event::Bell => SessionEvent::Bell,
                Event::Clipboard(text, primary) => SessionEvent::Clipboard(text, primary),
            })
            .collect();
        (events, next)
    }

    fn ended(&self) -> bool {
        self.live.ended()
    }
}

/// A finished run as the app records it: what it ran, where, how long, what it printed as text,
/// and what its parsers found.
fn finished_json(run: &crate::FinishedCommand) -> String {
    let output = String::from_utf8_lossy(&run.output);
    let plain = crate::parse::plain(&output);
    let items = crate::parse::items(&run.command, &run.output);
    let tool = crate::parse::tool_of(&run.command);
    let undoable = crate::undoability::of(&run.command);
    let value = serde_json::json!({
        "command": run.command,
        "cwd": run.cwd,
        "started_ms": run.started_ms,
        "ended_ms": run.ended_ms,
        "exit": run.exit,
        "output": plain,
        "truncated": run.truncated,
        "tool": tool,
        "undoable": undoable.unwrap_or(false),
        "number": run.number,
        "job": run.job,
        "refusal": run.refusal,
        "items": items,
    });
    value.to_string()
}

/// The sessions this device keeps, as the process's `ui.Terminal` nodes read them.
struct Sessions;

impl ui::terminal::Engine for Sessions {
    fn revision(&self) -> u64 {
        generation()
    }

    fn sessions(&self) -> ui::terminal::Sessions {
        attached()
            .into_iter()
            .map(|(id, live)| {
                let session: SessionHandle = Arc::new(LiveSession::new(live));
                (id, session)
            })
            .collect()
    }
}

/// Hand this process's live sessions to the toolkit: the windows it draws then show their
/// grids. Called when a session is first kept; later calls change nothing.
pub(crate) fn hand_to_toolkit() {
    static HANDED: std::sync::Once = std::sync::Once::new();
    HANDED.call_once(|| ui::terminal::set_engine(Arc::new(Sessions)));
}
