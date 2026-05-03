//! Hosted compositor mode — runs the shell inside the developer's existing
//! X11/Wayland session via `winit`. This is the legacy path that every
//! `cargo run -p quick-desktop` invocation has used; it was lifted verbatim
//! out of `main()` as part of Phase 1 of the real-Wayland-compositor plan so
//! the new `--mode native` dispatch has somewhere to land.
//!
//! Behaviour is unchanged from before the split.

use crate::shell::{
    build_nirios_root, persist, seed_demo_editor_presence, sync_tab_presence_from_tree,
};
use quick_ui::presence::PresenceKey;
use quick_ui::{App, State};

/// Boot the shell on top of the host's existing display server.
///
/// Mirrors the previous `main()` body verbatim: load (or default) the
/// persisted workspace tree, seed presence so collaboration chrome paints on
/// the first frame, and hand control to `App::run`, which drives the winit
/// event loop until the user quits.
pub fn run() {
    let path = persist::state_json_path();
    let tree = persist::load_or_default(&path);
    let state = State::new();
    let root = build_nirios_root(&tree);
    let mut app = App::new(state, root);
    // Seed per-tab presence from the tree's collaborator roster so
    // tab-pill presence dots render on first draw. Network-driven
    // presence updates later rewrite the same keys via the collab
    // pump, so this does not conflict with live peer attribution.
    sync_tab_presence_from_tree(&tree, app.presence_ctx_mut());
    // Seed a demo peer caret + selection inside the editor body so the
    // collab chrome in `EditorApp::draw` is visible in single-process
    // development. Network traffic replaces this with real peer state
    // when it arrives.
    seed_demo_editor_presence(app.presence_ctx_mut());
    // Declare the pointer-presence scope so local cursor moves broadcast
    // to every connected peer under the active workspace id. Remote
    // peers receive `CollabEvent::RemotePresenceUpdate` on the same
    // key and render this user's pointer on the correct workspace.
    app.set_presence_broadcast_scope(Some(PresenceKey::Workspace {
        id: format!("{}", tree.active.0),
    }));
    app.run();
}
