//! The window's side of a terminal: the live sessions it holds, as the toolkit's `ui.Terminal`
//! reads them.
//!
//! The grid, the pty and the runs a shell marks are the engine's ([`tilo_terminal::live`]); this
//! is the one translation between the engine's vocabulary and the toolkit's, so a widget draws a
//! grid without linking the engine and keys still go back through the engine's one key table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ui::key_input::{Button, Held, Key, Keys, Named};
use ui::terminal::{
    CellColor, GridPos, Line, Modes, Palette, Placement, Screen, Session, SessionEvent,
    SessionHandle, Style,
};

/// One live session as the toolkit sees it.
pub struct LiveSession {
    live: Arc<tilo_terminal::live::Live>,
    /// The grid converted for the frame being drawn, and the revision it was converted from.
    shown: Mutex<Option<(u64, Arc<Screen>)>>,
}

impl LiveSession {
    #[must_use]
    pub fn new(live: Arc<tilo_terminal::live::Live>) -> Self {
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

    /// The engine's live session behind this handle.
    #[must_use]
    pub fn live(&self) -> &Arc<tilo_terminal::live::Live> {
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

fn cell_color(color: tilo_terminal::CellColor) -> CellColor {
    use tilo_terminal::CellColor as Engine;
    match color {
        Engine::Default => CellColor::Default,
        Engine::Indexed(index) => CellColor::Indexed(index),
        Engine::Rgb(r, g, b) => CellColor::Rgb(r, g, b),
    }
}

fn modes(modes: tilo_terminal::TermModes) -> Modes {
    Modes {
        cursor_visible: modes.cursor_visible,
        bracketed_paste: modes.bracketed_paste,
        app_cursor_keys: modes.app_cursor_keys,
        mouse_report: modes.mouse_report,
        mouse_sgr: modes.mouse_sgr,
        focus_report: modes.focus_report,
    }
}

fn engine_key(key: Key) -> tilo_terminal::input::Key {
    use tilo_terminal::input as engine;
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

fn held(held: Held) -> tilo_terminal::input::Held {
    tilo_terminal::input::Held::new(held.shift, held.alt, held.ctrl, held.sup)
}

fn button(button: Button) -> tilo_terminal::input::Button {
    match button {
        Button::Left => tilo_terminal::input::Button::Left,
        Button::Middle => tilo_terminal::input::Button::Middle,
        Button::Right => tilo_terminal::input::Button::Right,
    }
}

impl Keys for LiveSession {
    fn key(&self, key: Key, text: Option<&str>, held_beside: Held) {
        let modes = self.live.modes();
        if let Some(bytes) = tilo_terminal::input::encode_key(
            engine_key(key),
            text,
            held(held_beside),
            modes,
        ) {
            let _ = self.live.write(&bytes);
        }
    }

    fn text(&self, text: &str) {
        let _ = self.live.write(text.as_bytes());
    }

    fn paste(&self, text: &str) {
        let bytes = tilo_terminal::input::encode_paste(text, self.live.modes());
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
        let Some(bytes) = tilo_terminal::input::encode_mouse(
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
            tilo_terminal::GridPos::new(anchor.row, anchor.col),
            tilo_terminal::GridPos::new(cursor.row, cursor.col),
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
        tilo_terminal::is_public_web_link(url)
    }

    fn events(&self, since: u64) -> (Vec<SessionEvent>, u64) {
        let (events, next) = self.live.events(since);
        let events = events
            .into_iter()
            .map(|event| match event {
                tilo_terminal::live::Event::Title(title) => SessionEvent::Title(title),
                tilo_terminal::live::Event::Finished(run) => {
                    SessionEvent::Finished(finished_json(&run))
                }
                tilo_terminal::live::Event::Exit(code) => SessionEvent::Exit(code),
                tilo_terminal::live::Event::Lost(why) => SessionEvent::Lost(why),
                tilo_terminal::live::Event::Bell => SessionEvent::Bell,
                tilo_terminal::live::Event::Clipboard(text, primary) => {
                    SessionEvent::Clipboard(text, primary)
                }
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
fn finished_json(run: &tilo_terminal::FinishedCommand) -> String {
    let output = String::from_utf8_lossy(&run.output);
    let plain = tilo_terminal::parse::plain(&output);
    let items = tilo_terminal::parse::items(&run.command, &run.output);
    let tool = tilo_terminal::parse::tool_of(&run.command);
    let undoable = tilo_terminal::undoability::of(&run.command);
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

/// The sessions a window holds, as the handles `ui.Terminal` reads: rebuilt when a session was
/// registered or released.
pub struct Handles {
    generation: Option<u64>,
    sessions: Arc<ui::terminal::Sessions>,
    inputs: Arc<ui::key_input::Inputs>,
}

impl Default for Handles {
    fn default() -> Self {
        Self {
            generation: None,
            sessions: Arc::new(HashMap::new()),
            inputs: Arc::new(HashMap::new()),
        }
    }
}

impl Handles {
    /// The handles now, made again from the engine's registry when it changed.
    pub fn now(&mut self) -> Arc<ui::terminal::Sessions> {
        self.refresh();
        Arc::clone(&self.sessions)
    }

    /// Where each session's keys go now: the same handles, by the id a view names.
    pub fn inputs(&mut self) -> Arc<ui::key_input::Inputs> {
        self.refresh();
        Arc::clone(&self.inputs)
    }

    fn refresh(&mut self) {
        let generation = tilo_terminal::live::generation();
        if self.generation == Some(generation) {
            return;
        }
        let mut sessions = HashMap::new();
        let mut inputs = HashMap::new();
        for (id, live) in tilo_terminal::live::attached() {
            let handle: SessionHandle = Arc::new(LiveSession::new(live));
            let keys: Arc<dyn ui::key_input::Keys> = handle.clone();
            inputs.insert(id.clone(), keys);
            sessions.insert(id, handle);
        }
        self.generation = Some(generation);
        self.sessions = Arc::new(sessions);
        self.inputs = Arc::new(inputs);
    }
}

/// The palette a terminal draws with, from the theme's own terminal colours.
#[must_use]
pub fn palette(theme: &ui::Theme) -> Palette {
    Palette::from_theme(theme)
}
