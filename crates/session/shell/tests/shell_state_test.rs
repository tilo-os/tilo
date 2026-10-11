#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::state::{
    AppKind, CapRole, Collaborator, Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceTree,
};

#[test]
fn default_tree_builds_three_workspaces_and_a_shell_root() {
    // Phase `workspace_tree_default_has_three_workspaces_each_with_one_strip`.
    {
        // `bootstrap_demo` seeds a 3-workspace fixture for the overview's 3-card
        // grid; pinned so seed drift surfaces as a failure, not a silent UX shift.
        let t = WorkspaceTree::bootstrap_demo();
        assert_eq!(t.desktop.workspaces.len(), 3);
        for ws in &t.desktop.workspaces {
            assert_eq!(
                ws.strips.len(),
                1,
                "workspace {:?} should have 1 strip",
                ws.name
            );
            assert!(
                t.desktop.active_strip.contains_key(&ws.id),
                "missing active_strip for {:?}",
                ws.name
            );
        }
    }

    // Phase `append_workspace_sets_active`.
    {
        let mut t = WorkspaceTree::bootstrap_demo();
        let new_id = t.append_empty_workspace("New");
        t.set_active(new_id);
        assert_eq!(t.desktop.active, new_id);
    }

    // Phase `tilo_shell_root_constructs_from_tree`.
    {
        use shell::shell::build_tilo_root;
        let tree = shell::shell::state::WorkspaceTree::bootstrap_demo();
        let _boxed: Box<dyn ui::widget::Widget> = build_tilo_root(&tree);
    }
}

#[test]
fn window_widths_clamp_migrate_and_round_trip_as_fractions() {
    // Phase `window_width_clamp`.
    {
        // A width is a fraction of the strip, so out-of-range fractions clamp
        // into the storable band rather than into a pixel band.
        assert_eq!(WindowWidth::Fraction(0).permille(), 1);
        assert_eq!(WindowWidth::Fraction(65_000).permille(), 10_000);
        // And the resolved pixels track the display, which is the point: a
        // group may be wider than the strip, which then scrolls.
        assert!((WindowWidth::Fraction(1500).resolve(1600.0) - 2400.0).abs() < 1e-9);
        assert!((WindowWidth::Fraction(1500).resolve(3840.0) - 5760.0).abs() < 1e-9);
    }

    // Phase `fraction_widths_round_trip_through_persisted_json`.
    // The current shape round-trips through the persisted JSON unchanged.
    {
        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.desktop.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Fraction(742);
        let json = serde_json::to_string(&tree).expect("serialize");
        let back: WorkspaceTree = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.desktop.workspaces[0].strips[0].window_groups[0].width,
            WindowWidth::Fraction(742),
        );
    }
}

// Reference the remaining imports so this compiles before the widget code
// that exercises them lands.
#[allow(dead_code, reason = "never called: only keeps the imports referenced")]
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
fn tree_persistence_is_atomic_and_a_missing_file_loads_the_default() {
    // Phase `persistence_roundtrip_atomic`.
    {
        use shell::shell::persist::{load_tree_from, save_tree_to};
        use shell::shell::state::WorkspaceTree;
        let dir = tempfile::tempdir().expect("temporary state directory");
        let tmp = dir.path().join("state.json");
        // `bootstrap_demo` is an intentionally recognised legacy seed and loading
        // it correctly migrates to the clean first-boot layout. Exercise atomic
        // persistence with ordinary user state instead of accidentally testing
        // against that migration contract.
        let mut t = WorkspaceTree::bootstrap_default();
        t.desktop.workspaces[0].name = "Round-trip workspace".into();
        save_tree_to(&t, &tmp).expect("save");
        let loaded = load_tree_from(&tmp).expect("load");
        assert_eq!(loaded.desktop.workspaces.len(), t.desktop.workspaces.len());
        assert_eq!(loaded.desktop.active, t.desktop.active);
        assert_eq!(
            loaded.desktop.workspaces[0].name,
            t.desktop.workspaces[0].name
        );
    }

    // Phase `load_missing_file_returns_default`.
    {
        use shell::shell::persist::load_or_default;
        use std::path::PathBuf;
        let missing = PathBuf::from("/tmp/definitely-missing-xyz.json");
        let _ = std::fs::remove_file(&missing);
        let (t, notice) = load_or_default(&missing);
        // `load_or_default` falls back to `bootstrap_default` when the persistence
        // file is absent — one empty workspace, not the named trio, so a first
        // boot isn't a claim about someone who never used the machine.
        assert_eq!(t.desktop.workspaces.len(), 1);
        // A first boot is not a refusal; reporting one would alarm every new machine.
        assert!(
            notice.is_none(),
            "a missing state file is the first boot, not something to warn about"
        );
    }
}
