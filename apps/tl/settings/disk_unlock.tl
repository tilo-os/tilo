/// The encrypted volumes enrolled for unlocking with the person's other devices: how each keeps
/// its half of the key, and which Wi-Fi networks early boot may join to ask them.
view disk_unlock_pane() {
    let disk = device.disk_unlock
    ui.Section("Encrypted volumes") {
        if not disk.scanned {
            ui.Text(
                "Looking for encrypted volumes asks this computer for its unlocking, which it keeps for whoever administers it.",
                role: .caption,
                secondary: true,
            )
            ui.Button("Look for encrypted volumes", icon: .search, role: .primary) {
                on press: device.refresh(device.disk_unlock)
            }
        } else if let error = disk.read_error {
            ui.StatusLine(error, status: .error)
            ui.Button("Try again", icon: .rotate_cw) {
                on press: device.refresh(device.disk_unlock)
            }
        } else if disk.volumes.is_empty() {
            ui.EmptyState("No encrypted volume is enrolled", icon: .lock, inline: true)
        }
    }
    for volume in disk.volumes {
        ui.Section("Volume {volume.volume}") {
            ui.PropertyList {
                ui.Property("Secrets", sealing_words(volume.sealing))
                ui.Property("Key slot", "{volume.keyslot}")
                ui.Property("Devices that may unlock it", unlocking_devices_words(volume))
            }
            if volume.sealing == .cleartext {
                ui.StatusLine(
                    "Anyone holding this disk can read the half kept on it.",
                    status: .warning,
                )
            }
        }
        ui.Section("Networks early boot may join") {
            unlock_network_rows(volume, disk.saved_networks)
        }
    }
    if let problem = disk.problem {
        ui.StatusLine(problem, status: .error)
    }
}

/// The Wi-Fi connections a volume may use at boot: the chosen ones and the ones saved here, each
/// with the control that adds or removes it from the volume's list.
view unlock_network_rows(volume: device.UnlockVolume, saved: List<Text>) {
    let offered = volume.networks + saved.filter(n -> not volume.networks.any(x -> x == n))
    if offered.is_empty() {
        ui.Text(
            "No Wi-Fi connection is saved on this computer yet.",
            role: .caption,
            secondary: true,
        )
    }
    for network in offered {
        let chosen = volume.networks.any(x -> x == network)
        ui.ListRow(
            network,
            secondary: if chosen {
                "Early boot may join it to reach your devices"
            } else { "Not used to unlock this volume" },
            selected: chosen,
            icon: .wifi,
        ) {
            trailing: {
                ui.Button(
                    if chosen { "Stop using it" } else { "Use it" },
                    size: .small,
                    toggled: chosen,
                ) {
                    on press: device.set_networks(
                        device.disk_unlock,
                        volume: volume.volume,
                        networks: if chosen {
                            volume.networks.filter(x -> x != network)
                        } else { volume.networks + [network] },
                    )
                }
            }
        }
    }
}

/// How a volume keeps the half of its key its siblings release.
fn sealing_words(sealing: device.Sealing) -> Text {
    if sealing == .tpm2 {
        "Sealed to this machine's chip"
    } else { "Kept in the clear on this disk" }
}

/// The person's devices that may release the half, or that none does yet.
fn unlocking_devices_words(volume: device.UnlockVolume) -> Text {
    if volume.siblings.is_empty() { "None yet" } else { volume.siblings.join(", ") }
}
