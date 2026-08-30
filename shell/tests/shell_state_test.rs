use shell::shell::state::{
    AppKind, CapRole, Collaborator, Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceTree,
};

#[test]
fn workspace_tree_default_has_three_workspaces_each_with_one_strip() {
    // `bootstrap_demo` seeds a 3-workspace fixture for the overview's 3-card
    // grid; pinned so seed drift surfaces as a failure, not a silent UX shift.
    let t = WorkspaceTree::bootstrap_demo();
    assert_eq!(t.workspaces.len(), 3);
    for ws in &t.workspaces {
        assert_eq!(
            ws.strips.len(),
            1,
            "workspace {:?} should have 1 strip",
            ws.name
        );
        assert!(
            t.active_strip.get(&ws.id).is_some(),
            "missing active_strip for {:?}",
            ws.name
        );
    }
}

#[test]
fn append_workspace_sets_active() {
    let mut t = WorkspaceTree::bootstrap_demo();
    let new_id = t.append_empty_workspace("New");
    t.set_active(new_id);
    assert_eq!(t.active, new_id);
}

#[test]
fn window_width_clamp() {
    // A width is a fraction of the strip, so out-of-range fractions clamp
    // into the legal share band rather than into a pixel band.
    assert_eq!(WindowWidth::Fraction(5).permille(), 150);
    assert_eq!(WindowWidth::Fraction(9999).permille(), 1000);
    // And the resolved pixels track the display, which is the point.
    assert_eq!(WindowWidth::Fraction(9999).resolve(1600.0), 1600.0);
    assert_eq!(WindowWidth::Fraction(9999).resolve(3840.0), 3840.0);
}

/// A workspace persisted by the pixel model still loads, its widths coming
/// back as the share of the screen the user had — not a pixel count that
/// would mean something different on their next monitor.
#[test]
fn legacy_pixel_widths_migrate_to_the_share_they_represented() {
    // The old model ran 300..1200 px with 1200 as the whole strip, so its
    // tier constants were a third / a half / two thirds of it.
    assert_eq!(WindowWidth::from_legacy_pixels(400).permille(), 333);
    assert_eq!(WindowWidth::from_legacy_pixels(600).permille(), 500);
    assert_eq!(WindowWidth::from_legacy_pixels(1200).permille(), 1000);

    let legacy = r#"{ "id": 7, "tabs": [], "active_tab": 0, "width": { "Custom": 600 } }"#;
    let g: WindowGroup = serde_json::from_str(legacy).expect("legacy width must load");
    assert_eq!(
        g.width,
        WindowWidth::Fraction(500),
        "a persisted 600 px width is half the strip, whatever the display",
    );
    assert_eq!(g.width.resolve(1600.0), 800.0);
    assert_eq!(g.width.resolve(3840.0), 1920.0);
}

/// The current shape round-trips through the persisted JSON unchanged.
#[test]
fn fraction_widths_round_trip_through_persisted_json() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Fraction(742);
    let json = serde_json::to_string(&tree).expect("serialize");
    let back: WorkspaceTree = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        back.workspaces[0].strips[0].window_groups[0].width,
        WindowWidth::Fraction(742),
    );
}

// Reference the remaining imports so this compiles before the widget code
// that exercises them lands.
#[allow(dead_code)]
fn _type_refs() {
    let _ = std::marker::PhantomData::<Workspace>;
    let _ = std::marker::PhantomData::<Strip>;
    let _ = std::marker::PhantomData::<WindowGroup>;
    let _ = std::marker::PhantomData::<Tab>;
    let _ = std::marker::PhantomData::<TileContent>;
    let _ = std::marker::PhantomData::<AppKind>;
    let _ = std::marker::PhantomData::<CapRole>;
    let _ = std::marker::PhantomData::<Collaborator>;
}

#[test]
fn persistence_roundtrip_atomic() {
    use shell::shell::persist::{load_tree_from, save_tree_to};
    use shell::shell::state::WorkspaceTree;
    let tmp = std::env::temp_dir().join(format!("qd-test-{}.json", std::process::id()));
    let t = WorkspaceTree::bootstrap_demo();
    save_tree_to(&t, &tmp).expect("save");
    let loaded = load_tree_from(&tmp).expect("load");
    assert_eq!(loaded.workspaces.len(), t.workspaces.len());
    assert_eq!(loaded.active, t.active);
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn load_missing_file_returns_default() {
    use shell::shell::persist::load_or_default;
    use std::path::PathBuf;
    let missing = PathBuf::from("/tmp/definitely-missing-xyz.json");
    let _ = std::fs::remove_file(&missing);
    let (t, notice) = load_or_default(&missing);
    // `load_or_default` falls back to `bootstrap_default` when the persistence
    // file is absent — one empty workspace, not the named trio, so a first
    // boot isn't a claim about someone who never used the machine.
    assert_eq!(t.workspaces.len(), 1);
    // A first boot is not a refusal; reporting one would alarm every new machine.
    assert!(
        notice.is_none(),
        "a missing state file is the first boot, not something to warn about"
    );
}

#[test]
fn nirios_shell_root_constructs_from_tree() {
    use shell::shell::build_nirios_root;
    let tree = shell::shell::state::WorkspaceTree::bootstrap_demo();
    let _boxed: Box<dyn ui::widget::Widget> = build_nirios_root(&tree);
}
