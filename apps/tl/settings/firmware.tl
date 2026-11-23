/// The firmware updates fwupd offers for this computer's devices and what it installed before,
/// read from the firmware resource and changed by its own actions.
view firmware_pane() {
    let fw = device.firmware
    ui.Section("Updates") {
        if not fw.available {
            ui.Text(
                "This computer has no firmware service, so it offers no updates here.",
                role: .caption,
                secondary: true,
            )
        } else if fw.updates.is_empty() {
            ui.EmptyState("No firmware update is offered", icon: .package, inline: true)
        }
        for update in fw.updates {
            ui.ListRow(
                update.device,
                secondary: firmware_words(update),
                icon: .package,
            )
        }
    }
    if not fw.updates.is_empty() {
        ui.Button(
            if fw.installing { "Installing firmware…" } else { "Install the updates" },
            icon: .download,
            role: .primary,
            enabled: not fw.installing,
        ) {
            on press: device.install(device.firmware)
        }
        ui.Text(
            "The devices may need a restart when the install finishes.",
            role: .caption,
            secondary: true,
        )
    }
    ui.Button("Read fwupd's list again", icon: .rotate_cw) {
        on press: device.refresh(device.firmware)
    }
    ui.Section("Before") {
        if fw.history.is_empty() {
            ui.Text("No firmware was installed on this computer yet.", role: .caption, secondary: true)
        }
        for entry in fw.history {
            ui.ListRow(
                entry.device,
                secondary: history_words(entry),
                icon: .history,
            )
        }
    }
    if let problem = fw.problem {
        ui.StatusLine(problem, status: .error)
    }
}

/// What an offered update changes, in a line.
fn firmware_words(update: device.FirmwareUpdate) -> Text {
    if let from = update.current_version {
        if let to = update.new_version {
            "{from} to {to}"
        } else {
            "A new version is offered"
        }
    } else if let to = update.new_version {
        "Version {to}"
    } else if let description = update.description {
        description
    } else {
        "A firmware update is offered"
    }
}

/// What one past install did, in a line.
fn history_words(entry: device.FirmwareHistoryEntry) -> Text {
    if let error = entry.error {
        "{entry.state}: {error}"
    } else if let from = entry.from_version {
        if let to = entry.to_version {
            "{entry.state}: {from} to {to}"
        } else {
            entry.state
        }
    } else {
        entry.state
    }
}
