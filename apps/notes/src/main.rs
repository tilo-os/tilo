//! Thin binary entry point. All logic lives in the library.
//!
//! No `--shots` mode yet — Phase 1 ships visual UI tests via the
//! `OffscreenRenderer` from integration tests, not from the binary
//! flag. We can add it the moment the sidebar grows interesting
//! enough chrome to warrant a CI snapshot.

use notes::NotesApp;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let app_widget = NotesApp::spawn_default()?;
    let state = ui::State::new();
    let app = ui::App::new(state, Box::new(app_widget));
    app.run();
    Ok(())
}
