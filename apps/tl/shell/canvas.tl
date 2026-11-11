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
///
/// A resource dragged out of a view — a message's attachment chip, a search hit, a row of
/// another view — and let go over the canvas is placed at the point it was dropped: the map
/// reports the drop with the canvas point under the pointer, and the shell runs the system's
/// `place`, which is what an agent's `place_at` runs too. The shell never handles the drop
/// itself, exactly as it never handles an attachment's press.
view canvas(a: Arrangement, name: Text, framed: Bool) {
    stack(gap: .related, fill) {
        canvas_entry(here_x(a), here_y(a))
        ui.Map(name: name, fill) {
            on drop(target: host.Resource, at: List<Int>): place_dropped(target, at)
            for s in a.strips {
                if s.placed {
                    for p in places_of(a, s.key) {
                        ui.MapPin(
                            name: p.key,
                            x: pin_x(a, s, p),
                            y: s.canvas_y,
                            width: pin_width(s),
                            height: pin_height(s),
                        ) {
                            if framed {
                                framed_place(a, p)
                            } else {
                                space_content(a, p)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Puts a resource dropped on the canvas where it was let go. A place that already stands on the
/// canvas — the place whose handle was dragged — moves to the point it was let go at, through
/// its own `position` action, so a drag that ends on the canvas moves the place and never stands
/// a second one. Any other resource — a message's attachment, a search hit, a row of another
/// view — is placed at that point through the system's own `place`, which is what an agent's
/// `place_at` runs. Either is one arranging step of the person's, attributed and undoable.
action place_dropped(target: host.Resource, at: List<Int>) (label: "Move it here") {
    let x = at.at(0) ?? 0
    let y = at.at(1) ?? 0
    if let held = place_showing(target.id()) {
        session.position(place: the_place(held.key), x: x, y: y)
    } else {
        session.place(resource: target, x: x, y: y)
    }
}

/// The place standing on the canvas that shows the resource `record`, if one does: what a drag
/// that ended on the canvas moves rather than placing a second time.
fn place_showing(record: Text) -> Option<ArrangementPlace> {
    let a = arrangement
    a.places.find(p -> p.record == record)
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
///
/// The handle stands in the frame's own bar, beside the shell's controls: a `ui.DragSource`
/// carrying the place's resource. Picked up there and let go over a composer, it attaches what
/// the place shows to that draft (shell.md §8.1, "Dropping on the composer"); let go over the
/// canvas, the map reports the drop and the place stands where it was let go. The body is never
/// a source: a press inside it stays the app's, so what a place shows — a field, a row, a
/// message — takes input as it always did.
view framed_place(a: Arrangement, p: ArrangementPlace) {
    ui.WindowFrame(name: place_title(p), fill) {
        controls: {
            place_controls(a, p)
        }
        body: {
            stack(gap: .none, fill) {
                place_handle(p)
                place_content(p)
            }
        }
    }
}

/// A place's handle: the slim bar a person picks the place up by, carrying the resource the
/// place shows, by the address the place records. It stands over the body, its own strip, so a
/// press inside the body stays the app's; its hand and the place's name say what it is. A place
/// showing no record (a foreign window) has no handle: there is nothing to carry.
view place_handle(p: ArrangementPlace) {
    if p.record != "" {
        ui.DragSource(p.record, title: place_title(p)) {
            row(gap: .tight, align: .center, fill, padding: .tight) {
                ui.Icon(.hand, size: .small)
                ui.Text("Move place", role: .caption, secondary: true)
                spacer
            }
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
template view canvas_entry(x: Int, y: Int) {
    state typed: Text = ""
    ui.TextInput(bind: typed, placeholder: "Find something, or ask") {
        on submit: session
            .place_found(text: typed, view: .none, strip: .none, x: x, y: y)
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
