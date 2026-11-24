/// The canvas's page: a map with a place at each point and a window on it as a traditional
/// desktop's does, and a map of the world with a route, markers and an area kept offline.
view places(g: Gallery) {
    scroll {
        stack(gap: .section, max: 72rem) {
            part(
                "Canvas",
                "A map that pans and zooms under the places on it, and a window standing on it.",
            ) {
                layers(aspect: 2.2) {
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
            }
            part(
                "Map of the world",
                "A route across the world, the places it passes and the area this device keeps offline.",
            ) {
                layers(aspect: 2.2) {
                    ui.Map(
                        name: "Berlin",
                        world: true,
                        latitude: 52.52,
                        longitude: 13.405,
                        zoom: 11.5,
                        hint: "A map of the world: its imagery, the route across it, and the places on it",
                    ) {
                        markers: {
                            ui.MapMarker(
                                name: "Museum Island",
                                latitude: 52.5169,
                                longitude: 13.401,
                                kind: .museum,
                            )
                            ui.MapMarker(
                                name: "Café at the corner",
                                latitude: 52.5235,
                                longitude: 13.4115,
                                kind: .cafe,
                            )
                            ui.MapMarker(
                                name: "Where this device is",
                                latitude: 52.52,
                                longitude: 13.405,
                                kind: .current,
                                selected: true,
                            )
                        }
                        tracks: {
                            ui.MapTrack(
                                name: "The way there",
                                points: [52.5169, 13.401, 52.52, 13.405, 52.5235, 13.4115],
                                tone: .walk,
                                width: 4.0,
                                dashed: true,
                            )
                        }
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
