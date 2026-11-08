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
                        ui.MapPin(
                            name: .some(p.key),
                            x: pin_x(a, s, p),
                            y: s.canvas_y,
                            width: pin_width(s),
                            height: pin_height(s),
                        ) {
                            framed_place(a, p)
                        }
                    }
                }
            }
        }
    }
}

/// The frameless canvas: every place at its point, drawn as its own view with no frame around it,
/// grouped into the strip's space by nearness.
view spaces_canvas(a: Arrangement) {
    stack(gap: .related, fill) {
        canvas_entry(a)
        ui.Map(name: .some("Spaces"), fill) {
            for s in a.strips {
                if s.placed {
                    for p in places_of(a, s.key) {
                        ui.MapPin(
                            name: .some(p.key),
                            x: pin_x(a, s, p),
                            y: s.canvas_y,
                            width: pin_width(s),
                            height: pin_height(s),
                        ) {
                            space_content(a, p)
                        }
                    }
                }
            }
        }
    }
}

/// How wide a strip's places stand on the canvas: the width the record keeps for it, so a window
/// that was dropped wide stays wide, and 720 when it kept none.
fn pin_width(s: ArrangementStrip) -> Int {
    if s.canvas_width > 0 { s.canvas_width } else { 720 }
}

/// Where the place `p` stands on the canvas: its strip's corner, then the places before it in the
/// strip laid end to end. The record keeps one canvas point on a strip, and the places of the strip
/// stand in a row from it (canvas.md §6), so a strip of several places is a run of work and none
/// stands over another.
fn pin_x(a: Arrangement, s: ArrangementStrip, p: ArrangementPlace) -> Int {
    let place = 0
    for q in places_of(a, s.key) {
        if q.key == p.key {
            return s.canvas_x + place * (pin_width(s) + pin_width(s) / 10)
        }
        place += 1
    }
    s.canvas_x
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
            place_content(p)
        }
    }
}

/// A place on the frameless canvas: its resource's view standing as content (or its window's
/// well), with a line naming what it is rather than a frame around it. The controls stand
/// outside it, and the content itself is `place_content`'s, as every preset draws it.
view space_content(a: Arrangement, p: ArrangementPlace) {
    stack(gap: .related, fill) {
        place_controls(a, p)
        ui.Text(place_title(p), role: .label, secondary: true)
        place_content(p)
    }
}

/// How tall a strip's places stand on the canvas: the height the record keeps for it, so a view
/// that fills its place (an editor, a mailbox) has the room it was placed with. A strip placed
/// before it kept one stands as tall as a desktop frame does.
fn pin_height(s: ArrangementStrip) -> Int {
    if s.canvas_height > 0 { s.canvas_height } else { 480 }
}

/// What the person types on the canvas: a search whose hit stands where they
/// typed, or a question their agent answers there (`shell.md` §8, §8.1).
/// `session.place_found` is one action: what the text finds is placed as its
/// primary view at the caret, one arranging step of the person's, attributed
/// to them and undone as one step; what the text asks is posted into their
/// conversation with the agent, and that conversation stands at the caret
/// instead. `x` and `y` are where the caret was, in canvas pixels.
view canvas_entry(a: Arrangement) {
    state typed: Text = ""
    ui.TextInput(bind: typed, placeholder: "Find something, or ask") {
        on submit: session
            .place_found(
                text: typed,
                view: .none,
                strip: .none,
                x: .some(here_x(a)),
                y: .some(here_y(a)),
            )
            .then(set(typed, ""))
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
