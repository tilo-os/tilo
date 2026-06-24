//! Thin binary entry point. All logic lives in the library.
//!
//! Collaboration: set `QUICK_COLLAB_SOCK` to a running `collab-broker`
//! socket and `QUICK_APP_ID` to a per-instance identity, and two
//! instances share one document with live presence:
//!
//! ```bash
//! collab-broker /tmp/notes.sock &
//! QUICK_COLLAB_SOCK=/tmp/notes.sock QUICK_APP_ID=alice notes &
//! QUICK_COLLAB_SOCK=/tmp/notes.sock QUICK_APP_ID=bob   notes
//! ```

use notes::NotesApp;
use ui::presence::PresenceKey;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Per-peer identity. The same id drives (a) the Automerge actor so
    // two instances are distinct CRDT authors, (b) the broker's presence
    // attribution, and (c) the per-peer presence colour. A missing
    // QUICK_APP_ID falls back to a random id so two un-configured
    // instances still get distinct actors.
    let app_id = std::env::var("QUICK_APP_ID")
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());

    let mut app_widget = NotesApp::spawn_default()?;
    // Attribute this instance's edits to `app_id` so peers can highlight
    // "what they wrote" in this user's color.
    app_widget.set_local_author(Some(app_id.clone()));
    let mut state = ui::State::new();
    state
        .doc
        .set_actor(automerge::ActorId::from(app_id.as_bytes()));

    let mut app = ui::App::new(state, Box::new(app_widget));
    // Announce ourselves under a stable workspace scope: this drives the
    // late-join handshake (peers reply with full state) and is the scope
    // under which our pointer presence is broadcast.
    app.set_presence_broadcast_scope(Some(PresenceKey::Workspace {
        id: "notes".to_string(),
    }));
    app.run();
    Ok(())
}
