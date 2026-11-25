/// Every app that can reach a file or folder, and what each one reaches.
///
/// It is the same view Files draws for a file, grouped by app: a workspace's grant on a file's
/// node, or this device's record of a file chosen outside any workspace. Taking one back writes
/// the workspace's change or the device's record, as whoever gave it.
view apps_access_pane() {
    let entries = workspace.access.workspaces.flat_map(w -> w.grants)
    if entries.is_empty() {
        ui.EmptyState(
            "No app can reach a file or folder",
            icon: .app_window,
            message: "A file you give an app in the file chooser is listed here.",
        )
    }
    for g in entries.group(entry -> entry.app).sort_by(g -> g.key) {
        ui.Section(if let first = g.items.first() { first.holder } else { g.key }) {
            files.access_list(entries: g.items.sort_by(e -> e.path), empty: "")
        }
    }
}
