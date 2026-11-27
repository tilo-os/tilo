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
        ui.ToggleGroup(selected: work_index(s.work.from), label: "Whose work this device takes") {
            ui.Segment("Nobody") {
                on press: device.set_work(device.server, "nobody")
            }
            ui.Segment("My devices") {
                on press: device.set_work(device.server, "own-devices")
            }
            ui.Segment("Anyone whose workspace it holds") {
                on press: device.set_work(device.server, "anyone")
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button(
                if s.work.when.charging_only {
                    "Taking work only while charging"
                } else { "Taking work while on the battery" },
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
                on change(v): set_processors_room(v)
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
                on change(v): set_memory_room(v)
            }
        }
    }
    ui.Section("Services this device offers") {
        service_row(
            s.services.app_serving,
            "Serve apps",
            "Run the workspace's apps for devices that cannot",
            "app_serving",
        )
    }
    let web = s.web
    ui.Section("The web page it serves") {
        if let domain = web.domain {
            ui.PropertyList {
                ui.Property("Domain name", domain)
            }
        } else {
            ui.StatusLine(
                "Browsers outside your workspace will warn that this server's certificate is not trusted.",
                status: .warning,
            )
            ui.Text(
                "Set a domain name for this server to fix that.",
                role: .caption,
                secondary: true,
            )
        }
    }
}

/// One service of this device, with what offering it means and the control that offers or
/// withdraws it.
view service_row(offered: Bool, title: Text, about: Text, service: Text) {
    setting(title, about) {
        ui.Button(if offered { "Offered" } else { "Not offered" }, toggled: offered, role: .plain) {
            on press: device.set_service(device.server, service, not offered)
        }
    }
}

/// Where the person's choice stands in the work chooser: nobody, their devices, or anyone.
fn work_index(from: device.WorkFrom) -> Int {
    if from == .own-devices { 1 } else if from == .anyone { 2 } else { 0 }
}

/// The processors this device keeps free for the people at it: the slider's share, made whole.
action set_processors_room(percent: Float) (label: "Keep processors free") {
    device.set_work_room(device.server, "processors", percent.round())
}

/// The memory this device keeps free for the people at it.
action set_memory_room(percent: Float) (label: "Keep memory free") {
    device.set_work_room(device.server, "memory", percent.round())
}
