//! The window's side of a terminal: the sessions its `ui.Terminal` nodes read.
//!
//! A terminal's grid, its pty and its key table are the engine's, and the engine installs itself
//! as the toolkit's [`ui::terminal::Engine`], so what a window does is hand its nodes the table
//! of sessions the engine keeps, as a page is shown with its engine. The table is made again
//! whenever a session was opened or released.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_ui_base::{key_input::Inputs, key_input::Keys};
use ui::terminal::{Palette, Sessions};

/// The sessions a window holds, as the handles `ui.Terminal` reads: rebuilt when a session was
/// registered or released.
pub struct Handles {
    generation: Option<u64>,
    sessions: Arc<Sessions>,
    inputs: Arc<Inputs>,
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
    /// The handles now, made again from the engine when its set of sessions changed.
    pub fn now(&mut self) -> Arc<Sessions> {
        self.refresh();
        Arc::clone(&self.sessions)
    }

    /// Where each session's keys go now: the same handles, by the id a view names.
    pub fn inputs(&mut self) -> Arc<Inputs> {
        self.refresh();
        Arc::clone(&self.inputs)
    }

    fn refresh(&mut self) {
        let Some(engine) = ui::terminal::engine() else {
            return;
        };
        let generation = engine.revision();
        if self.generation == Some(generation) {
            return;
        }
        let sessions: Sessions = engine.sessions();
        let mut inputs = Inputs::new();
        for (id, session) in &sessions {
            let handle = Arc::clone(session);
            let keys: Arc<dyn Keys> = handle;
            inputs.insert(id.clone(), keys);
        }
        self.generation = Some(generation);
        self.sessions = Arc::new(sessions);
        self.inputs = Arc::new(inputs);
    }
}

/// The palette a terminal draws with, from the theme's own terminal colours.
#[must_use]
pub fn palette(theme: &tilo_ui_base::Theme) -> Palette {
    Palette::from_theme(theme)
}
