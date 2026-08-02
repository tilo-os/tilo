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
use notes::shots::{self, DEFAULT_SHOTS_VIEWPORT};
use ui::presence::PresenceKey;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Headless snapshot mode: `--shots <file.png>` (optionally with
    // `--viewport WxH`) renders one offscreen frame and exits, so the
    // whole UI can be reviewed without a window.
    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_flag(&args, "--shots") {
        let viewport = parse_flag(&args, "--viewport")
            .and_then(|v| shots::parse_viewport(&v))
            .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        // `--scenario <name>` drives the app into an interaction state
        // before the snapshot: `drawer` (narrow page drawer), `drag`
        // (mid-drag ghost + reflow), `menu` (row context menu), `slash`
        // (editor block menu).
        let scenario = parse_flag(&args, "--scenario");
        shots::render_to_path(
            viewport,
            std::path::Path::new(&out),
            scenario.as_deref(),
        )?;
        return Ok(());
    }

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
    // Register Notes' action decoders so the Ctrl+K palette and remote replay
    // can decode every page-tree mutation this app dispatches.
    notes::actions::register(app.action_registry_mut());
    // Announce ourselves under a stable workspace scope: this drives the
    // late-join handshake (peers reply with full state) and is the scope
    // under which our pointer presence is broadcast.
    app.set_presence_broadcast_scope(Some(PresenceKey::Workspace {
        id: "notes".to_string(),
    }));
    app.run();
    Ok(())
}

/// Read `--name value` or `--name=value` out of `args`. Returns the
/// value string if present.
fn parse_flag(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == name {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&prefix) {
            return Some(rest.to_string());
        }
    }
    None
}
