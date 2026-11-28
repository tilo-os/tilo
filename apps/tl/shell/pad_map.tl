/// A drawing tablet's pad map, as the session publishes it (`device.pad_map`): while the person
/// holds the pad's map button or one of its mode buttons, what each button, ring and strip does
/// now — the focused app's own word for it, or Tilo's mapping.
///
/// Where the pad's model is known, the controls stand at the edges of a drawing of the pad, each
/// where it sits on the pad in the person's hand; otherwise they are listed, a keycap and what it
/// does. It stands in the middle of the screen, over everything, and takes neither the pointer nor
/// the keyboard: the person reads it with the button held and goes on. It fades in as the button
/// goes down and out as it comes up. Nothing while no button holds it.
view pad_map_card() {
    let p = device.pad_map
    layers(fill, transition: .cross_fade) {
        if p.visible {
            row(fill, align: .center, distribute: .center, padding: .group) {
                ui.Card(title: p.name, accessible_name: p.spoken) {
                    stack(gap: .related) {
                        ui.Text(p.hint, role: .caption, secondary: true)
                        if p.drawn {
                            pad_drawn(p)
                        } else {
                            ui.KeyHints {
                                for c in p.controls {
                                    ui.KeyHint([c.cap], c.does)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The controls around a drawing of the pad: those along its top edge above it, its left and
/// right edges beside it, its bottom edge below, each in the order it sits along its edge.
view pad_drawn(p: device.PadMap) {
    stack(gap: .related, align: .center) {
        pad_edge_row(p.top)
        row(gap: .section, align: .center) {
            pad_edge_column(p.left)
            ui.Drawing(
                [
                    ui.Shape.rect(
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                        round: 0.12,
                        fill: .some(.surface),
                    ),
                    ui.Shape.rect(
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                        round: 0.12,
                        stroke: .some(.track),
                        weight: .regular,
                    ),
                    ui.Shape.rect(
                        x: 0.18,
                        y: 0.16,
                        w: 0.64,
                        h: 0.68,
                        round: 0.12,
                        fill: .some(.track),
                    ),
                ],
                meaning: .decoration,
                label: "",
                width: 168.0,
                height: 120.0,
            )
            pad_edge_column(p.right)
        }
        pad_edge_row(p.bottom)
    }
}

/// The controls along the left or right edge, top to bottom.
view pad_edge_column(along: List<device.PadMapControl>) {
    if not along.is_empty() {
        ui.KeyHints {
            for c in along {
                ui.KeyHint([c.cap], c.does)
            }
        }
    }
}

/// The controls along the top or bottom edge, left to right.
view pad_edge_row(along: List<device.PadMapControl>) {
    if not along.is_empty() {
        row(gap: .section, wrap: true, distribute: .center) {
            for c in along {
                ui.KeyHint([c.cap], c.does)
            }
        }
    }
}
