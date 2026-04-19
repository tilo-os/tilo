use quick_desktop::shell::{build_nirios_root, persist};
use quick_ui::{App, State};

fn main() {
    tracing_subscriber::fmt::init();
    let path = persist::state_json_path();
    let tree = persist::load_or_default(&path);
    let state = State::new();
    let root = build_nirios_root(&tree);
    let app = App::new(state, root);
    app.run();
}
