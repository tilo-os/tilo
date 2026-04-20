use quick_desktop::shell::{
    build_nirios_root, persist, seed_demo_editor_presence, sync_tab_presence_from_tree,
};
use quick_ui::presence::PresenceKey;
use quick_ui::{App, State};

fn main() {
    tracing_subscriber::fmt::init();
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
