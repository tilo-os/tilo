/// The limits this workspace's owners set on what members post there: the largest file a host
/// keeps, and what one member's files may take, now and in any hour. Every host of the workspace
/// keeps to them; a limit changed once applies on every host, and the content it refused is no
/// longer refused once it allows it.
view content_pane() {
    let c = workspace.content
    let owner = (invoker.role() ?? "") == "owner"
    state editing: Bool = false
    state file_mib: Float = 0.0
    state member_mib: Float = 0.0
    state hourly_mib: Float = 0.0
    ui.Section("What the host keeps") {
        ui.Text(
            "The limits every host of this workspace keeps to. A file over them stays with the person who posted it, and the file still opens from their device.",
            role: .caption,
            secondary: true,
        )
        setting("Largest file", "A bigger file is not kept by the host") {
            row(gap: .related, align: .center) {
                ui.Text(std.mib(c.file_bytes), role: .label)
                if owner {
                    ui.Button("Change") {
                        on press: set(file_mib, c.file_bytes.to_float() / 1048576.0)
                            .then(set(member_mib, c.member_bytes.to_float() / 1048576.0))
                            .then(set(hourly_mib, c.hourly_bytes.to_float() / 1048576.0))
                            .then(set(editing, true))
                    }
                }
            }
        }
        setting("One member's files", "What one member's files may take together") {
            ui.Text(std.mib(c.member_bytes), role: .label)
        }
        setting("One member an hour", "What one member may post in any hour") {
            ui.Text(std.mib(c.hourly_bytes), role: .label)
        }
        if not owner {
            ui.StatusLine("Only the workspace's owners can change these.", status: .idle)
        }
    }
    if owner and editing {
        ui.Section("New limits") {
            row(gap: .related, wrap: true) {
                ui.NumberInput(
                    bind: file_mib,
                    label: "Largest file",
                    min: 1.0,
                    max: 4096.0,
                    step: 5.0,
                    suffix: "MiB",
                )
                ui.NumberInput(
                    bind: member_mib,
                    label: "One member's files",
                    min: 1.0,
                    max: 65536.0,
                    step: 50.0,
                    suffix: "MiB",
                )
                ui.NumberInput(
                    bind: hourly_mib,
                    label: "One member an hour",
                    min: 1.0,
                    max: 65536.0,
                    step: 50.0,
                    suffix: "MiB",
                )
            }
            row(gap: .related, align: .center) {
                ui.Button("Save limits", role: .primary) {
                    on press: workspace
                        .set_policy(
                            workspace.content,
                            file_bytes: mib_bytes(file_mib),
                            member_bytes: mib_bytes(member_mib),
                            hourly_bytes: mib_bytes(hourly_mib),
                        )
                        .then(set(editing, false))
                }
                ui.Button("Cancel") {
                    on press: set(editing, false)
                }
            }
        }
    }
}

/// `mib` whole mebibytes as bytes, which is what the workspace's limits are set in.
fn mib_bytes(mib: Float) -> Int {
    (mib * 1048576.0).round()
}
