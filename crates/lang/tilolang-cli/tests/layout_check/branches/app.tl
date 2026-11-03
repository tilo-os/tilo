app so.tilo.lang.layoutbranches "Branches" (icon: .layout_grid, category: .utility)

type Sample {
    name: Text (title)
    /// A note the sample may hold; the scenario leaves it out.
    note: Option<Text>
}

root Sample

default view samples for Sample

/// The window: the sample's name, its note, and a line about it.
view samples(s: Sample) {
    stack {
        ui.Text(s.name, role: .heading)
        row(fill) {
            note_line(s)
        }
        name_line(s)
    }
}

/// A view whose whole body is a bare `if let` the scenario leaves absent: it runs and draws
/// nothing, so the row it stands in holds no widget of it, and it is still reached.
view note_line(s: Sample) {
    if let n = s.note {
        ui.Text(n)
    }
}

/// A view whose whole body is a bare `if let` holding only a call of another view: what it
/// draws is that view's, and it is still reached.
view name_line(s: Sample) {
    if let n = some_name(s) {
        name_text(n)
    }
}

/// The name, as a view of its own.
view name_text(n: Text) {
    ui.Text(n)
}

fn some_name(s: Sample) -> Option<Text> {
    .some(s.name)
}

/// A sample with a name and no note.
scenario "a sample" {
    let s = root Sample
    s.name = "Sample"
}
