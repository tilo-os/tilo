/// Bluetooth: the radio, the paired accessories and the devices in reach, as the device
/// publishes them. A press connects, disconnects, pairs or forgets through the accessory's own
/// action, so quick settings, this page and an agent change the same bond.
view bluetooth_pane() {
    state renaming: Text = ""
    state draft: Text = ""
    state forgetting: Text = ""
    let b = device.bluetooth
    ui.Section("Bluetooth") {
        if not b.available {
            ui.Text("This device has no Bluetooth", role: .caption, secondary: true)
        } else {
            ui.ListRow(
                "Bluetooth",
                secondary: if b.on { "On" } else { "Off" },
                icon: if b.on { .bluetooth } else { .bluetooth_off },
            ) {
                trailing: {
                    ui.Button(if b.on { "On" } else { "Off" }, toggled: b.on) {
                        on press: device.set_bluetooth(device.bluetooth, not b.on)
                    }
                }
            }
            if not b.on {
                ui.Text("Bluetooth is off", role: .caption, secondary: true)
            } else {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Button(
                        if b.discovering { "Stop looking" } else { "Look for devices" },
                        icon: .search,
                    ) {
                        on press: device.scan(device.bluetooth, on: not b.discovering)
                    }
                    if b.discovering {
                        ui.Spinner(label: "Looking for devices", beside: .caption)
                    }
                }
                if b.accessories.is_empty() {
                    ui.Text("No accessory is paired", role: .caption, secondary: true)
                }
                for accessory in b.accessories {
                    ui.ListRow(
                        accessory.name,
                        secondary: if accessory.connected { "Connected" } else { "Paired" },
                        selected: accessory.connected,
                        icon: .headphones,
                        value: if let battery = accessory.battery { "{battery}%" },
                    ) {
                        on activate: if accessory.connected {
                            accessory.disconnect()
                        } else { accessory.connect() }
                        trailing: {
                            row(gap: .tight, align: .center) {
                                ui.Button(
                                    if accessory.connected { "Disconnect" } else { "Connect" },
                                    size: .small,
                                ) {
                                    on press: if accessory.connected {
                                        accessory.disconnect()
                                    } else { accessory.connect() }
                                }
                                ui.Button(
                                    if accessory.trusted {
                                        "Connects by itself"
                                    } else { "Asks first" },
                                    size: .small,
                                    toggled: accessory.trusted,
                                ) {
                                    on press: accessory.set_trusted(not accessory.trusted)
                                }
                                ui.Button(
                                    "Rename this device",
                                    icon: .pencil,
                                    icon_only: true,
                                    role: .plain,
                                    size: .small,
                                ) {
                                    on press: set(draft, accessory.name)
                                        .then(
                                            set(renaming, accessory.address),
                                        )
                                }
                                ui.Button(
                                    "Forget this device",
                                    icon: .trash,
                                    icon_only: true,
                                    role: .plain,
                                    size: .small,
                                ) {
                                    on press: set(forgetting, accessory.address)
                                }
                            }
                        }
                    }
                    ui.ModalDialog(
                        title: "Name this device",
                        secondary: "Cancel",
                        primary: "Rename",
                        open: renaming == accessory.address,
                    ) {
                        on act_primary: accessory.rename(draft).then(set(renaming, ""))
                        on act_secondary: set(renaming, "")
                        on dismiss: set(renaming, "")
                        ui.TextInput(bind: draft, placeholder: "What this device is called")
                    }
                    ui.ModalDialog(
                        title: "Forget {accessory.name}?",
                        secondary: "Cancel",
                        primary: "Forget",
                        destructive: true,
                        open: forgetting == accessory.address,
                    ) {
                        on act_primary: accessory.forget().then(set(forgetting, ""))
                        on act_secondary: set(forgetting, "")
                        on dismiss: set(forgetting, "")
                        ui.Text(
                            "It must pair again to connect, and its bond and keys are removed.",
                            role: .caption,
                        )
                    }
                }
                if not b.nearby.is_empty() {
                    ui.Text("In reach", role: .caption, secondary: true)
                    for nearby in b.nearby {
                        ui.ListRow(
                            nearby.name,
                            secondary: if let signal = nearby.signal {
                                "{signal} dBm"
                            } else { "In reach" },
                            icon: .bluetooth,
                        ) {
                            on activate: nearby.pair()
                            trailing: {
                                ui.Button("Pair", size: .small, role: .primary) {
                                    on press: nearby.pair()
                                }
                            }
                        }
                    }
                } else if b.accessories.is_empty() {
                    ui.EmptyState("No devices found", icon: .bluetooth, inline: true)
                }
            }
        }
        if let problem = b.problem {
            ui.StatusLine(problem, status: .error)
        }
    }
}
