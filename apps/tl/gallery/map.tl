/// The canvas's page: a map filling it, with a place at each point and a window on it as a
/// traditional desktop's does.
view places(g: Gallery) {
    ui.Map(name: "Places") {
        ui.MapPin(name: "Notes", x: 40, y: 40, width: 240) {
            ui.Card(title: "Notes") {
                ui.Text("A place on the canvas.", role: .caption, secondary: true)
            }
        }
        ui.MapPin(name: "Tasks", x: 320, y: 160, width: 240) {
            ui.Card(title: "Tasks") {
                ui.Text("Another, beside it.", role: .caption, secondary: true)
            }
        }
        ui.MapPin(name: "Content", x: 620, y: 40, width: 320, height: 200) {
            ui.Place(name: "Groceries", fill) {
                controls: {
                    ui.Button("Close", icon: .x, icon_only: true, role: .plain)
                }
                body: {
                    stack(gap: .related, padding: .section) {
                        ui.Text(
                            "A resource's view on the canvas: no frame, its handle on hover.",
                            role: .caption,
                            secondary: true,
                        )
                    }
                }
            }
        }
        ui.MapPin(name: "Window", x: 80, y: 300, width: 360, height: 200) {
            ui.WindowFrame(name: "A window", focused: true, fill) {
                controls: {
                    ui.Button("Close", icon: .x, icon_only: true, role: .bar)
                }
                body: {
                    stack(gap: .related, padding: .section) {
                        ui.Text(
                            "A window on the shell: its bar, its controls, what it shows.",
                            role: .caption,
                            secondary: true,
                        )
                    }
                }
            }
        }
    }
}

/// The canvas group, with two places on it.
scenario "places" {
    root Gallery
}

/// Screens arranged by dragging: this device's panel beside a desk monitor, each named and
/// numbered as Identify shows them.
view screens(g: Gallery) {
    ui.PlacementCanvas(name: "Screens", identify: true) {
        ui.PlacementScreen(
            key: "eDP-1",
            name: "Laptop",
            number: 1,
            x: 0,
            y: 0,
            width: 1440,
            height: 900,
            primary: true,
            movable: true,
        )
        ui.PlacementScreen(
            key: "HDMI-A-1",
            name: "Desk monitor",
            number: 2,
            x: 1440,
            y: 0,
            width: 2560,
            height: 1440,
            movable: true,
        )
    }
}

scenario "screens" {
    root Gallery
}
