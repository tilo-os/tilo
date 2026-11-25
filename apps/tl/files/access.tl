/// Who and which apps may reach one thing, written once and shown wherever a person asks about
/// access: Files draws it for the file or folder in front, and Settings draws it for each app.
///
/// A row is one holder — a person, an agent or an app — with what it may do, the face of whoever
/// gave it, and, where the person may take it back, the control that does. Both kinds of grant are
/// the same list: one on a workspace's file node, and one this device keeps for a file chosen
/// outside any workspace. Taking either back runs `workspace.access.revoke`, which writes the
/// workspace's change or the device's record as the person.
template view access_list(entries: List<workspace.AccessEntry>, empty: Text) {
    if entries.is_empty() {
        ui.Text(empty, role: .caption, secondary: true)
    } else {
        for entry in entries {
            ui.ListRow(
                entry.holder,
                secondary: entry.allows,
                icon: if entry.holder_kind == "app" {
                    .app_window
                } else if entry.holder_kind == "agent" { .sparkles } else { .user },
            ) {
                trailing: {
                    row(gap: .related, align: .center, wrap: true) {
                        if entry.granted_by_name != "" {
                            row(gap: .tight, align: .center) {
                                ui.Avatar(name: entry.granted_by_name)
                                text(
                                    if entry.granted_by_you {
                                        "You gave this"
                                    } else { "{entry.granted_by_name} gave this" },
                                    role: .caption,
                                    secondary: true,
                                )
                            }
                        }
                        if entry.can_revoke {
                            ui.Button("Take back", role: .destructive, size: .small) {
                                on press: workspace.access.revoke(id: entry.id)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The grants that reach `path`: one on the thing itself, or one on a folder above it. A grant on
/// a folder reaches everything under it, so a file inside a shared folder says so.
fn access_reaching(path: Text) -> List<workspace.AccessEntry> {
    workspace.access.workspaces
        .flat_map(w -> w.grants)
        .filter(
            entry -> entry.path != ""
                and (entry.path == path or path.starts_with(entry.path + "/")),
        )
        .sort_by(entry -> entry.holder)
}
