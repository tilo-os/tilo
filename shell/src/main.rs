use quick_desktop::shell::{build_nirios_root, persist, sync_tab_presence_from_tree};
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
    app.run();
}
