use quick_desktop::shell::state::{
    AppKind, Collaborator, Role, Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceTree,
};

#[test]
fn workspace_tree_default_has_one_workspace_and_one_strip() {
    let t = WorkspaceTree::bootstrap_default();
    assert_eq!(t.workspaces.len(), 1);
    assert_eq!(t.workspaces[0].strips.len(), 1);
    assert!(t.active_strip.get(&t.workspaces[0].id).is_some());
}

#[test]
fn append_workspace_sets_active() {
    let mut t = WorkspaceTree::bootstrap_default();
    let new_id = t.append_empty_workspace("New");
    t.set_active(new_id);
    assert_eq!(t.active, new_id);
}

#[test]
fn window_width_clamp() {
    assert_eq!(WindowWidth::Custom(50).pixels(), 300);
    assert_eq!(WindowWidth::Custom(9999).pixels(), 1200);
}

// Reference the remaining imports so the test file compiles cleanly even before
// the widget/layout code that exercises them lands in later tasks.
#[allow(dead_code)]
fn _type_refs() {
    let _ = std::marker::PhantomData::<Workspace>;
    let _ = std::marker::PhantomData::<Strip>;
    let _ = std::marker::PhantomData::<WindowGroup>;
    let _ = std::marker::PhantomData::<Tab>;
    let _ = std::marker::PhantomData::<TileContent>;
    let _ = std::marker::PhantomData::<AppKind>;
    let _ = std::marker::PhantomData::<Role>;
    let _ = std::marker::PhantomData::<Collaborator>;
}

#[test]
fn persistence_roundtrip_atomic() {
    use quick_desktop::shell::persist::{save_tree_to, load_tree_from};
    use quick_desktop::shell::state::WorkspaceTree;
    let tmp = std::env::temp_dir().join(format!("qd-test-{}.json", std::process::id()));
    let t = WorkspaceTree::bootstrap_default();
    save_tree_to(&t, &tmp).expect("save");
    let loaded = load_tree_from(&tmp).expect("load");
    assert_eq!(loaded.workspaces.len(), t.workspaces.len());
    assert_eq!(loaded.active, t.active);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn load_missing_file_returns_default() {
    use quick_desktop::shell::persist::load_or_default;
    use std::path::PathBuf;
    let missing = PathBuf::from("/tmp/definitely-missing-xyz.json");
    let _ = std::fs::remove_file(&missing);
    let t = load_or_default(&missing);
    assert_eq!(t.workspaces.len(), 1);
}

#[test]
fn nirios_shell_root_constructs_from_tree() {
    use quick_desktop::shell::build_nirios_root;
    let tree = quick_desktop::shell::state::WorkspaceTree::bootstrap_default();
    let _boxed: Box<dyn quick_ui::widget::Widget> = build_nirios_root(&tree);
}
