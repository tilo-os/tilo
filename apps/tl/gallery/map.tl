/// What the gallery holds of a canvas: places at points on a surface that pans and zooms.
type Places {
    title: Text
}

default view places for Places

/// The canvas's window: a map filling it, as a canvas stands, with a place at each point and a
/// window standing on it as a traditional desktop's does.
view places(p: Places) {
    ui.Map(name: p.title) {
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
        ui.MapPin(name: "Window", x: 80, y: 300, width: 360, height: 200) {
            ui.WindowFrame(name: "A window", focused: true, fill) {
                controls: {
                    ui.Button("Close", icon: .x, icon_only: true, role: .bar)
                }
                body: {
                    ui.Text(
                        "A window on the desk: its bar, its controls, what it shows.",
                        role: .caption,
                        secondary: true,
                    )
                }
            }
        }
    }
}

/// The canvas group, with two places on it.
scenario "places" {
    let g = root Gallery
    g.places.insert(Places(title: "Places"))
}
