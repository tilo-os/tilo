app so.tilo.lang.layoutsurface "Surface" (icon: .layout_grid, category: .utility)

/// A note a person may ask about where they point.
type Note {
    title: Text
}

root board: Note

default view canvas for Note

/// The canvas: a surface a person points into, and what they asked at the point they pointed.
view canvas(n: Note) {
    state pointed: List<Int> = []
    ui.Map(name: "Canvas", ground: true, fill) {
        on press(at: List<Int>): set(pointed, at)
        if pointed.len() > 0 {
            ui.MapPin(
                name: "Caret",
                x: pointed.at(0) ?? 0,
                y: pointed.at(1) ?? 0,
                width: 240,
                height: 80,
            ) {
                asked(n)
            }
        }
    }
}

/// The page only a point on the surface shows.
view asked(n: Note) {
    ui.Text("{n.title}, asked here")
}

scenario "a board" {
    let n = root Note
    n.title = "Plans"
}
