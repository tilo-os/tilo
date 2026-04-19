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
