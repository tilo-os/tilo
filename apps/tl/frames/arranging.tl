/// What the person does with frames, and what their arrangement does (shell.md §4, §5).
///
/// Arranging is the person's own: their input, or a view acting on it, and never a task
/// (workspace-model.md §7). Each action below changes the person's arrangement record, so every
/// move, split and close is attributed, in history and undoable like any other change.
///
/// A resource reached by reference — a row in a list, a link in a message, an agent's answer —
/// lands by the person's setting: a new frame, the frame split beside it, or a tab in the frame
/// it came from (shell.md §3).
/// How a reference lands in a shell of frames: the person's setting (shell.md §3).
enum Opening {
    /// A frame of its own, beside the one it came from.
    new_frame
    /// The frame it came from, split: a second frame in the same column, side by side.
    split
    /// A tab in the frame it came from.
    tab
}

/// Brings a place to the front of its frame: its tab chosen.
action bring_to_front(f: Frame, place: Place) {
    if not f.places.any(x -> x == place) {
        return
    }
    f.front = .some(place)
}

/// Closes a place's tab: the frame shows the tab that was beside it, or the frame itself goes
/// when it held only that place.
///
/// Closing removes the place from the person's arrangement. It never touches the resource and
/// nobody else's arrangement (workspace-model.md §4). `next` is the tab the view showed beside
/// it, which is what the person then sees; the view reads it before closing.
action close_place(f: Frame, place: Place, next: Place) {
    if not f.places.any(x -> x == place) {
        return
    }
    f.places.remove(place)
    if f.places.is_empty() {
        f.front = .none
    } else if f.places.any(x -> x == next) {
        f.front = .some(next)
    } else if let first = f.places.first() {
        f.front = .some(first)
    }
}

/// Moves a place's tab within its frame, to just after the place `after` names, or to the end
/// when the drop names none of its frame's places.
action move_place(f: Frame, place: Place, after: Place, to_end: Bool) {
    if not f.places.any(x -> x == place) {
        return
    }
    if after == place {
        return
    }
    if not to_end and f.places.any(x -> x == after) { f.places.move(place, after: after) } else { f.places.move(place, to: f.places.len()) }
    f.front = .some(place)
}

/// Hands a place's tab to the frame it was dragged onto (`onto`), in front there, so the person
/// lands where they dropped it.
///
/// The tab leaves its frame the way its close does; a frame that held only it goes. `next` is
/// the tab its old frame then shows.
action move_place_to(f: Frame, place: Place, onto: Frame, next: Place) {
    if f == onto {
        return
    }
    if not f.places.any(x -> x == place) {
        return
    }
    f.places.remove(place)
    if f.places.is_empty() {
        f.front = .none
    } else if f.places.any(x -> x == next) {
        f.front = .some(next)
    } else if let first = f.places.first() {
        f.front = .some(first)
    }
    onto.places.insert(place)
    onto.front = .some(place)
}

/// Opens a resource a reference names: by the person's `Opening` setting a frame of its own,
/// the frame split, or a tab in the frame it came from.
///
/// A resource the arrangement already shows is brought to its frame instead: opening something
/// shown is going to it (workspace-model.md §4), never a second frame of it.
action open_reference(arrangement: Arrangement, strip: Strip, from: Stack, shows: host.Resource, opening: Opening) {
    if let frame = find_frame(arrangement, shows) {
        frame.focused = true
        return
    }
    let made = Place(shows: shows, shows_as: "", window_state: "")
    match opening {
        .tab => if let frame = from.frames.find(inside -> inside.focused) ?? from.frames.first() {
            frame.places.insert(made)
            frame.front = .some(made)
        },
        .split => {
            let frame = from.frames.insert(Frame())
            frame.places.insert(made)
            frame.front = .some(made)
            frame.focused = true
        }
        .new_frame => {
            let column = strip.stacks.insert(Stack())
            let frame = column.frames.insert(Frame())
            frame.places.insert(made)
            frame.front = .some(made)
            frame.focused = true
        }
    }
}

/// The frame of `arrangement` that already shows `shows`, if any: opening something shown goes
/// to it.
fn find_frame(arrangement: Arrangement, shows: host.Resource) -> Option<Frame> {
    for strip in arrangement.strips {
        for column in strip.stacks {
            for frame in column.frames {
                if frame.places.any(inside -> inside.shows == shows) {
                    return .some(frame)
                }
            }
        }
    }
    .none
}

/// Widens a frame in its strip: one step of the arrangement's widths, never past the strip's
/// own (layout.md, "Every screen": a frame keeps working at the width it is given).
action widen(f: Frame, to: Int = f.width + 250) {
    f.width = to
}

/// Narrows a frame in its strip: one step, never under the narrowest that still works.
action narrow(f: Frame, to: Int = f.width - 250) {
    f.width = to
}

/// Maximizes a frame over its strip: it takes the room, and its siblings give way.
action maximize(f: Frame) {
    f.maximized = true
}

/// Brings a maximized frame back to the width it had in its strip.
action restore(f: Frame) {
    f.maximized = false
}

/// Closes a frame and every place in it: each leaves the person's arrangement, as its own
/// close would.
action close_frame(f: Frame) {
    for place in f.places {
        f.places.remove(place)
    }
    f.front = .none
}

/// Splits a frame: the place in front moves to a frame of its own in the same column, so the
/// person sees two places of one frame side by side.
action split_frame(f: Frame, into: Stack) {
    if f.places.len() < 2 {
        return
    }
    let front = f.front ?? f.places.first()
    if let front = front {
        f.places.remove(front)
        if let first = f.places.first() {
            f.front = .some(first)
        } else {
            f.front = .none
        }
        let made = into.frames.insert(Frame())
        made.places.insert(front)
        made.front = .some(front)
        made.focused = true
    }
}

/// Expands a frame in its stack: its header chosen, its content shown, the others' headers
/// kept above it.
action expand_frame(stack: Stack, f: Frame) {
    if not stack.frames.any(x -> x == f) {
        return
    }
    stack.expanded = .some(f)
    f.focused = true
}

/// Brings a strip's column to the front of the strip: the column the person works in.
action focus_column(strip: Strip, column: Stack) {
    if not strip.stacks.any(x -> x == column) {
        return
    }
    strip.front = .some(column)
}

/// Brings a strip to the front of the arrangement: the workspace they were working in.
action focus_strip(arrangement: Arrangement, strip: Strip) {
    if not arrangement.strips.any(x -> x == strip) {
        return
    }
    arrangement.front = .some(strip)
}
