app so.tilo.lang.layoutlayered "Layered" (icon: .layout_grid, category: .utility)

type Sample {
    name: Text (title)
}

root Sample

default view samples for Sample

/// A view whose whole body is one `layers`: its content fills the window and a row stands over
/// its foot, as the shell's desk lays its dock over the desk. The row covers the content on
/// purpose, and the text of each stands apart from the other's, so neither is an overlap.
view samples(s: Sample) {
    layers {
        stack(fill) {
            ui.Text("First", role: .heading)
        }
        row(at: .bottom, fill, align: .center) {
            ui.Text(s.name)
            ui.Button("Second") {
                on press: rename(s)
            }
        }
    }
}

/// Rename the sample: what the row's button does.
action rename(s: Sample) {
    s.name = "Renamed"
}

/// A sample with a name.
scenario "a sample" {
    let s = root Sample
    s.name = "Sample"
}
