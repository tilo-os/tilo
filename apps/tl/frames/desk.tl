/// The desk: the strips the person had open, each a row of columns, each column a stack of
/// frames with one expanded (shell.md §4, workspace-model.md §6).
///
/// The desk is the shell's root view over the person's arrangement. It draws the strip in
/// front as its columns; a column is a stack, which shows each frame's header above the one
/// expanded; and the expanded frame shows its place's content. Nothing on the desk comes from
/// an app: the frames say what their resources and types say.
/// The desk: the strip in front, its columns side by side.
///
/// A strip with no column says so, and offers nothing to open of its own: what opens is a
/// reference the person activates, which the shell's `Opening` setting places (shell.md §3).
view desk(a: Arrangement) {
    let front = a.front ?? a.strips.first()
    if let front = front {
        strip_row(front)
    } else {
        ui.EmptyState("Nothing is open", message: "Open a note, a conversation or a task, and it appears here.", icon: .layers)
    }
}

/// The strip in front as its columns, side by side: wide ones keep their width and a narrow
/// output flows them one under another, so a phone shows the same strip readable.
view strip_row(strip: Strip) {
    if strip.stacks.is_empty() {
        ui.EmptyState("The strip is empty", message: "A place opens in the column here.", icon: .columns_2)
    } else {
        row(gap: .related, align: .stretch, wrap: true) {
            for column in strip.stacks {
                stack_column(strip, column)
            }
        }
    }
}

/// One column of a strip: its stacks, each a frame's header above the expanded frame's content.
///
/// A column holds the frames that share it; the strips arrangement puts one frame per column
/// until the person stacks them, so a column of one shows that frame whole.
view stack_column(strip: Strip, column: Stack) {
    stack(gap: .related) {
        if strip.front == .some(column) {
            stack_frames(column, focused: true)
        } else {
            stack_frames(column, focused: false)
        }
    }
}

/// A stack: the frames whose headers stand above the expanded one, and the expanded frame's
/// content under them.
///
/// Each frame shows its own tab strip as its header, so every place in the stack stays
/// reachable; the expanded frame shows its front place's content. Activating another frame's
/// header expands it, the frames above sliding out of its way.
view stack_frames(column: Stack, focused: Bool) {
    let expanded = column.expanded ?? column.frames.first()
    stack(gap: .related) {
        for f in column.frames {
            if expanded == .some(f) {
                frame_open(column, f, focused: focused)
            } else {
                frame_folded(column, f)
            }
        }
    }
}

/// A frame in its stack, open: its head, and its front place's content under it.
///
/// The frame's content is the front place's resource through its type's view, so a stack shows
/// the same thing a frame alone does. A frame with no place says so instead of drawing an
/// empty head.
view frame_open(column: Stack, f: Frame, focused: Bool) {
    let front = f.front ?? f.places.first()
    if let front = front {
        frame_head(f, front.shows)
        place_content(front, focused: focused)
    } else {
        frame_empty(f)
    }
}

/// A frame folded into its header: the stack shows only its head, so the column keeps every
/// frame's places reachable while one is open. Activating its head expands it.
view frame_folded(column: Stack, f: Frame) {
    let front = f.front ?? f.places.first()
    if let front = front {
        stack {
            row(gap: .related, align: .center) {
                frame_head(f, front.shows)
                spacer
                ui.Button("Expand", icon: .chevron_down, icon_only: true, role: .plain) {
                    on press: expand_frame(column, f)
                }
            }
        }
    } else {
        frame_empty(f)
    }
}

/// A frame with no place: it says so, and offers close.
view frame_empty(f: Frame) {
    row(gap: .related, align: .center) {
        ui.Text("An empty frame", role: .caption, secondary: true)
        spacer
        ui.Button("Close the frame", icon: .x, icon_only: true, role: .plain) {
            on press: close_frame(f)
        }
    }
}

/// The front place's content: the resource through its type's view.
///
/// The shell shows the resource as its type shows it, with the window's own state from the
/// place; a place whose resource is not here says what it is instead of drawing nothing.
view place_content(p: Place, focused: Bool) {
    stack(gap: .section) {
        ui.Text(meta(p.shows).title, role: .large_title)
        ui.Text(meta(p.shows).type_name, role: .caption, secondary: true)
        ui.Button("Show this place", icon: .external_link) {
            on press: open(p.shows)
        }
    }
}
