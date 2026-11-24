/// What this device moved over each of its connections, the day a mobile period starts on and
/// the plan's allowance. The session counts it whatever window is open; a person starts the
/// mobile count again or forgets a connection's history from here.
view data_usage_pane() {
    let usage = settings.network_usage
    ui.Section("This device") {
        setting("Mobile plan's allowance", "Gigabytes in the period, where the plan has one") {
            ui.NumberInput(
                bind: usage.limit_gb,
                label: "Gigabytes",
                min: 0.0,
                max: 1000.0,
                step: 1.0,
            )
        }
        ui.Text(
            "The counts are this device's own: another device counts its own traffic.",
            role: .caption,
            secondary: true,
        )
    }
    ui.Section("Connections") {
        if usage.counters.keys().is_empty() {
            ui.EmptyState("Nothing has been counted yet", icon: .bar_chart, inline: true)
        }
        for key in usage.counters.keys() {
            if let counted = usage.counters.get(key) {
                ui.ListRow(
                    connection_name(usage, key),
                    secondary: "{readable_bytes(counted.received)} received, {readable_bytes(counted.sent)} sent",
                    icon: .bar_chart,
                ) {
                    trailing: {
                        row(gap: .tight, align: .center, wrap: true) {
                            ui.Button("Start again", size: .small) {
                                on press: settings.reset_data_used(
                                    settings.network_usage,
                                    connection: key,
                                )
                            }
                            ui.Button(
                                "Forget its history",
                                icon: .trash,
                                icon_only: true,
                                role: .destructive,
                                size: .small,
                            ) {
                                on press: settings.forget_data_used(
                                    settings.network_usage,
                                    connection: key,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What a connection is called: the saved network's name, else the key it is counted under.
fn connection_name(usage: settings.NetworkUsage, key: Text) -> Text {
    if let connection = usage.connections.get(key) {
        connection.name
    } else if key == "" { "Every connection" } else { key }
}

/// A number of bytes as a person reads it: the largest whole unit that fits.
fn readable_bytes(bytes: Int) -> Text {
    let thousand = 1000
    let million = 1000000
    let billion = 1000000000
    if bytes >= billion {
        "{bytes / billion} GB"
    } else if bytes >= million {
        "{bytes / million} MB"
    } else if bytes >= thousand { "{bytes / thousand} kB" } else { "{bytes} B" }
}
