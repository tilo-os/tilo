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
/// The map fills the desk under the entry: it cuts off what it holds at its edge, so a map only
/// as tall as its content would show none of it.
view framed_canvas(a: Arrangement) {
    stack(gap: .related, fill) {
        canvas_entry(a)
        ui.Map(name: .some("Canvas"), fill) {
            for s in a.strips {
                if s.placed {
                    for p in places_of(a, s.key) {
                        ui.MapPin(name: .some(p.key), x: s.canvas_x, y: s.canvas_y, width: s.canvas_width, height: pin_height(s)) {
                            framed_place(a, p)
                        }
                    }
                }
            }
        }
    }
}

/// The frameless canvas: every place at its point, drawn as its own view standing as content,
/// with no frame around it — nearness groups it into the strip's space.
view spaces_canvas(a: Arrangement) {
    stack(gap: .related, fill) {
        canvas_entry(a)
        ui.Map(name: .some("Spaces"), fill) {
            for s in a.strips {
                if s.placed {
                    for p in places_of(a, s.key) {
                        ui.MapPin(name: .some(p.key), x: s.canvas_x, y: s.canvas_y, width: s.canvas_width, height: pin_height(s)) {
                            space_content(a, p)
                        }
                    }
                }
            }
        }
    }
}

/// A place on the framed canvas: the shell's frame around it (the window frame a desktop's
/// windows stand in, its controls at the end of the bar), and its resource's view composed
/// inside, filling it, or the well its window shows in (shell.md §8). The toolbar stands
/// outside it.
view framed_place(a: Arrangement, p: ArrangementPlace) {
    ui.WindowFrame(name: .some(place_title(p)), fill) {
        controls: {
            place_controls(a, p)
        }
        body: {
            place_view(p, fill)
        }
    }
}

/// A place on the frameless canvas: its resource's view standing as content (or its window's
/// well), with a line naming what it is rather than a frame around it. The controls stand
/// outside it.
view space_content(a: Arrangement, p: ArrangementPlace) {
    stack(gap: .related, fill) {
        place_controls(a, p)
        ui.Text(if p.title == "" { p.shows } else { p.title }, role: .label, secondary: true)
        place_view(p, fill)
    }
}

/// How tall a strip's places stand on the canvas: the height the record keeps for it, so a view
/// that fills its place (an editor, a mailbox) has the room it was placed with. A strip placed
/// before it kept one stands as tall as a desktop frame does.
fn pin_height(s: ArrangementStrip) -> Int {
    if s.canvas_height > 0 { s.canvas_height } else { 480 }
}

/// What the person types on the canvas: a search that becomes a place. Typing a word or two and
/// pressing Enter runs the one `find` the palette and an agent's `tilo` tool use, and the top
/// result stands at the caret as its primary view (shell.md §8): one arranging action on the
/// person's arrangement, attributed to them and undone as one step. `x` and `y` are where the
/// caret was, in canvas pixels.
view canvas_entry(a: Arrangement) {
    lens typed: Text = ""
    ui.TextInput(value: typed, placeholder: "Find something and place it here") {
        on submit: session.place_found(text: typed, view: .none, x: .some(here_x(a)), y: .some(here_y(a)))
    }
}

/// Where text typed on the canvas puts what it finds: the canvas point the
/// first place stands at, so a new place lands where the person looks rather
/// than over what is already there. Zero on an empty canvas.
fn here_x(a: Arrangement) -> Int {
    if let first = a.places.first() {
        for s in a.strips {
            if s.key == first.strip and s.placed {
                return s.canvas_x
            }
        }
    }
    0
}

/// See [`here_x`].
fn here_y(a: Arrangement) -> Int {
    if let first = a.places.first() {
        for s in a.strips {
            if s.key == first.strip and s.placed {
                return s.canvas_y
            }
        }
    }
    0
}
