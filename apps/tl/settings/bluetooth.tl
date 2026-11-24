/// Bluetooth: the radio, the paired accessories and the devices in reach, as the device
/// publishes them. A press connects, disconnects, pairs or forgets through the accessory's own
/// action, so quick settings, this page and an agent change the same bond.
view bluetooth_pane() {
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
                                    "Forget this device",
                                    icon: .trash,
                                    icon_only: true,
                                    role: .plain,
                                    size: .small,
                                ) {
                                    on press: accessory.forget()
                                }
                            }
                        }
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
