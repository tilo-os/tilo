/// The NTFS volumes attached to this computer and the drives the person chose to remember: how
/// each opens, where it is open, and the actions that open, close, forget and reconfigure them.
view drives_pane() {
    let drives = device.drives
    ui.Section("Attached drives") {
        if let error = drives.read_error {
            ui.StatusLine(error, status: .error)
        } else if drives.drives.is_empty() {
            ui.EmptyState("No drive is attached", icon: .hard_drive, inline: true)
        }
        for drive in drives.drives {
            ui.ListRow(drive.label, secondary: drive_words(drive), icon: .hard_drive) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        if let mounted = drive.mounted {
                            ui.Button("Close", size: .small) {
                                on press: device.unmount(device.drives, drive: drive.id)
                            }
                        } else {
                            ui.Button("Open", size: .small, role: .primary) {
                                on press: device.mount(
                                    device.drives,
                                    drive: drive.id,
                                    safely: drive.preselect == .open_safely,
                                    remember: false,
                                )
                            }
                            ui.Button("Open safely", size: .small) {
                                on press: device.mount(
                                    device.drives,
                                    drive: drive.id,
                                    safely: true,
                                    remember: false,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
    ui.Section("Remembered drives") {
        if drives.remembered.is_empty() {
            ui.Text(
                "A drive this computer does not know is asked about each time it is attached.",
                role: .caption,
                secondary: true,
            )
        }
        for remembered in drives.remembered {
            ui.ListRow(
                remembered.label,
                secondary: if remembered.mode == .open_safely {
                    "Opens safely when attached; still asked about"
                } else {
                    "Opens with the faster driver when attached"
                },
                icon: .shield,
            ) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        ui.Button(
                            if remembered.mode == .open_safely {
                                "Open safely"
                            } else { "Open fast" },
                            size: .small,
                            toggled: remembered.mode == .open_safely,
                        ) {
                            on press: device.set_mode(
                                device.drives,
                                grant: remembered.grant,
                                safely: remembered.mode != .open_safely,
                            )
                        }
                        ui.Button(
                            "Forget this drive",
                            icon: .trash,
                            icon_only: true,
                            role: .destructive,
                            size: .small,
                        ) {
                            on press: device.forget(device.drives, grant: remembered.grant)
                        }
                    }
                }
            }
        }
    }
    if not drives.serving {
        ui.Text(
            "This computer's drive service is not running; drives cannot be opened here.",
            role: .caption,
            secondary: true,
        )
    }
    if let problem = drives.problem {
        ui.StatusLine(problem, status: .error)
    }
}

/// What a drive is, in a line: its size, where it sits, and whether it is open.
fn drive_words(drive: device.Drive) -> Text {
    let size = drive_size_words(drive.size_bytes)
    let where_ = if drive.placement == .external { "external" } else { "inside this computer" }
    if let mounted = drive.mounted {
        let path = mounted.path
        if mounted.read_only != .none {
            "{size}, {where_} - open at {path}, read-only"
        } else { "{size}, {where_} - open at {path}" }
    } else if drive.needs_consent {
        "{size}, {where_} - asked about before it opens"
    } else { "{size}, {where_} - not open" }
}

/// How many bytes read to a person: `4 GB`, `812 MB`.
fn drive_size_words(n: Int) -> Text {
    if n < 1000 {
        "{n} B"
    } else if n < 1000000 {
        "{n / 1000} KB"
    } else if n < 1000000000 { "{n / 1000000} MB" } else { "{n / 1000000000} GB" }
}
