/// The canvases of shell.md §2 and §8: places at points on a surface that pans and zooms, each
/// drawn in a frame (`frames`), or as its own view standing as content and grouped by where it
/// is into a space per task (`spaces`).
///
/// Pan and zoom are the map component's (`ui.Map`) camera: each person's lens, kept on their
/// device, never in the arrangement. What this file gives the map is where every place stands,
/// in canvas pixels. The record keeps a canvas place on each strip (`canvas_x`, `canvas_y`,
/// `canvas_width`, `canvas_height`); the places of a strip stand in a row from that corner, so a
/// strip of several windows is one run of work and a strip of one is one window where it was
/// dropped (canvas.md §6).
/// The framed canvas: every strip at its point, its places standing side by side from there.
/// Panning and zooming is the map's camera (a middle drag, Ctrl and a wheel, the arrow keys, two
/// fingers); it moves the lens and nothing in the arrangement, so a place stays where it was
/// while the person looks around, and the places keep their canvas points.
view framed_canvas(a: Arrangement) {
    ui.Map(name: .some("Canvas")) {
        for s in a.strips {
            if s.placed {
                for p in places_of(a, s.key) {
                    ui.MapPin(name: .some(p.key), x: s.canvas_x, y: s.canvas_y, width: s.canvas_width) {
                        framed_place(a, p)
                    }
                }
            }
        }
    }
}

/// The frameless canvas: every place at its point, drawn as its own view standing as content,
/// with no frame around it — nearness groups it into the strip's space.
view spaces_canvas(a: Arrangement) {
    ui.Map(name: .some("Spaces")) {
        for s in a.strips {
            if s.placed {
                for p in places_of(a, s.key) {
                    ui.MapPin(name: .some(p.key), x: s.canvas_x, y: s.canvas_y, width: s.canvas_width) {
                        space_content(a, p)
                    }
                }
            }
        }
    }
}

/// A place on the framed canvas: the shell's frame around it, and the well its window shows in
/// (shell.md §8). The toolbar stands outside the well, so the window fills the rest.
view framed_place(a: Arrangement, p: ArrangementPlace) {
    ui.Card(title: if p.title == "" { p.shows } else { p.title }) {
        place_content(a, p)
    }
}

/// A place on the frameless canvas: its window's well, with a line naming what it is rather
/// than a frame around it. The controls stand outside the well.
view space_content(a: Arrangement, p: ArrangementPlace) {
    stack(gap: .related) {
        place_controls(a, p)
        ui.Text(if p.title == "" { p.shows } else { p.title }, role: .label, secondary: true)
        ui.WindowWell(place: p.key, min: 12rem)
    }
}
