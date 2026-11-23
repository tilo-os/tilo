/// This device as a server: the work it takes from your other devices, what it keeps free so
/// the people at it never wait, and the services it offers. The daemon keeps these, so every
/// window and every device reads the same settings.
view device_server_pane() {
    let s = device.server
    ui.Section("This device as a server") {
        if let problem = s.problem {
            ui.StatusLine(problem, status: .error)
        }
        ui.Text(
            "What this device takes and offers to your other devices. A device without a server configuration writes one when you change something here.",
            role: .caption,
            secondary: true,
        )
        ui.ToggleGroup(
            selected: work_index(s.work.from),
            label: "Whose work this device takes",
        ) {
            ui.Segment("Nobody")
            ui.Segment("My devices")
            ui.Segment("Anyone whose workspace it holds")
            on change(i): device.set_work(device.server, work_name(i))
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button(
                if s.work.when.charging_only {
                    "Taking work only while charging"
                } else {
                    "Taking work while on the battery"
                },
                icon: .battery_charging,
                toggled: s.work.when.charging_only,
            ) {
                on press: device.set_work_while_charging(
                    device.server,
                    not s.work.when.charging_only,
                )
            }
        }
    }
    ui.Section("Kept free for the people at this device") {
        row(gap: .related, align: .center) {
            ui.Icon(.cpu)
            ui.Slider(
                value: s.work.when.free_cpu_percent.to_float(),
                label: "Processors kept free",
                min: 0.0,
                max: 100.0,
                step: 5.0,
                shows_value: true,
            ) {
                on change(v): device.set_work_room(device.server, "processors", v.round())
            }
        }
        row(gap: .related, align: .center) {
            ui.Icon(.database)
            ui.Slider(
                value: s.work.when.free_memory_percent.to_float(),
                label: "Memory kept free",
                min: 0.0,
                max: 100.0,
                step: 5.0,
                shows_value: true,
            ) {
                on change(v): device.set_work_room(device.server, "memory", v.round())
            }
        }
    }
    ui.Section("Services this device offers") {
        service_row(
            s.services.workspace_replication,
            "Replicate workspaces",
            "Keep a copy of your workspaces so they are there when another device asks",
            "workspace_replication",
        )
        service_row(
            s.services.inbound_server,
            "Accept connections",
            "Let your other devices reach this one directly",
            "inbound_server",
        )
        service_row(
            s.services.remote_control_server,
            "Remote control",
            "Let your other devices show and act on this device's desktop",
            "remote_control_server",
        )
        service_row(
            s.services.app_serving,
            "Serve apps",
            "Run the workspace's apps for devices that cannot",
            "app_serving",
        )
        service_row(
            s.services.compute,
            "Take compute",
            "Run the work your other devices send",
            "compute",
        )
    }
}

/// One service of this device, with what offering it means and the control that offers or
/// withdraws it.
view service_row(offered: Bool, title: Text, about: Text, service: Text) {
    setting(
        title,
        about,
        ui.Button(
            if offered { "Offered" } else { "Not offered" },
            toggled: offered,
            role: .plain,
        ) {
            on press: device.set_service(device.server, service, not offered)
        },
    )
}

/// Where the person's choice stands in the work chooser: nobody, their devices, or anyone.
fn work_index(from: Text) -> Int {
    if from == "own-devices" {
        1
    } else if from == "anyone" {
        2
    } else {
        0
    }
}

/// The work setting the chooser's segment `index` makes.
fn work_name(chosen: Int) -> Text {
    if chosen == 1 {
        "own-devices"
    } else if chosen == 2 {
        "anyone"
    } else {
        "nobody"
    }
}
