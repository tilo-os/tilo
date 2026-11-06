/// What the gallery holds of a canvas: places at points on a surface that pans and zooms.
type Places {
    title: Text
}

default view places for Places

/// The canvas's window: a map filling it, as a canvas stands, with a place at each point and a
/// window standing on it as a traditional desktop's does.
view places(p: Places) {
    ui.Map(name: .some(p.title)) {
        ui.MapPin(name: .some("Notes"), x: .some(40), y: .some(40), width: .some(240)) {
            ui.Card(title: "Notes") {
                ui.Text("A place on the canvas.", role: .caption, secondary: true)
            }
        }
        ui.MapPin(name: .some("Tasks"), x: .some(320), y: .some(160), width: .some(240)) {
            ui.Card(title: "Tasks") {
                ui.Text("Another, beside it.", role: .caption, secondary: true)
            }
        }
        ui.MapPin(
            name: .some("Window"),
            x: .some(80),
            y: .some(300),
            width: .some(360),
            height: .some(200),
        ) {
            ui.WindowFrame(name: .some("A window"), focused: true, fill) {
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
