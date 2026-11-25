/// Every app that can reach a file or folder, and what each one reaches.
///
/// It is the same view Files draws for a file, grouped by app: a workspace's grant on a file's
/// node, or this device's record of a file chosen outside any workspace. Taking one back writes
/// the workspace's change or the device's record, as whoever gave it. The list is the platform's
/// virtual one, so ten thousand grants show only the rows in view.
view apps_access_pane() {
    let entries = workspace.access.workspaces.flat_map(w -> w.grants)
    if entries.is_empty() {
        ui.EmptyState("No app or person was given a file.", icon: .app_window)
    } else {
        files.access_list(
            entries: entries.sort_by(e -> e.path).sort_by(e -> e.holder),
            empty: "",
            by_app: true,
        )
    }
}
