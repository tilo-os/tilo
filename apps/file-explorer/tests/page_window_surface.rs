// A page shows Files through a window of its own, whose resource a workspace decides by the
// app's committed surface: one missing from it is held by the workspace for good, and every
// other device and person finds no resource to open the window on.
#[test]
fn the_apps_surface_carries_the_pages_window_resource() {
    let report = ui::framework::headless::surface_report::<file_explorer::FileExplorerShell>();
    let kinds: Vec<&str> = report["resources"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|resource| resource["kind"].as_str())
        .collect();
    for kind in ["files.folder", "files.tree", "files.workspace"] {
        assert!(kinds.contains(&kind), "{kind} in {kinds:?}");
    }
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "files.workspace")
            .count(),
        1,
        "{kinds:?}"
    );
}
