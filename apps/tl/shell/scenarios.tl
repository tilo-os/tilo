/// The scenarios the shell's views are checked and shot against: real arrangements of real
/// resources from Notes, Tasks and Chat, at the sizes and counts people actually keep. They
/// never touch a person's own arrangement; a real first launch starts empty.
///
/// A scenario fills the arrangement's origin collections, which the session's reading would
/// otherwise write: the records are the same shapes, so what the views read of a scenario is
/// what they read of a real desk. The shell's own record (`viewing`) says which way it shows
/// them; switching it keeps every place where it was.
/// Adds a workspace, with one row, to `a`, and returns the row's key.
action a_workspace(a: Arrangement, name: Text) -> Text {
    let w = a.workspaces.insert(ArrangementWorkspace(key: "w-{name}", name: name))
    let r = a.rows.insert(ArrangementRow(key: "r-{name}", workspace: w.key, name: name, shell: "so.tilo.shell.strips"))
    r.key
}

/// Adds a strip named `name` to `row`, standing at `width`, and returns its key.
action a_strip(a: Arrangement, row: Text, name: Text, width: Int) -> Text {
    let s = a.strips.insert(ArrangementStrip(key: "s-{row}-{name}", workspace: "w-Desk", row: row, width: width, shell: "so.tilo.shell.strips"))
    s.key
}

/// Adds a place showing the note at `index` of the notebook to `strip`, and returns its key.
action a_note_place(a: Arrangement, strip: Text, index: Int) -> Text {
    let chosen = notes.notes.take(index + 1)
    if let n = chosen.last() {
        let p = a.places.insert(ArrangementPlace(key: "p-{index}", strip: strip, shows: "so.tilo.lang.notes", record: n.id(), title: n.title, shell: "so.tilo.shell.strips"))
        return p.key
    }
    ""
}

/// A place of a kind this build has no record for: it stands as it is, which is what a place
/// another shell made looks like (§2).
action a_foreign_place(a: Arrangement, strip: Text, index: Int, title: Text) -> Text {
    let p = a.places.insert(ArrangementPlace(key: "f-{index}", strip: strip, shows: "so.tilo.terminal", title: title, shell: "so.tilo.shell.other"))
    p.key
}

/// A working desk's places at the counts people keep: one place on its own, three across a
/// narrow list, a wide board and a usual note, then a busy day as three strips of four. The
/// strips stand side by side in one row, and the narrow list holds focus.
scenario "a desk of places" -> Arrangement {
    let v = viewing
    v.showing = .strips
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
    let one = a_strip(a, row, "One", 667)
    a_note_place(a, one, 0)
    let narrow = a_strip(a, row, "List", 333)
    a.places.insert(ArrangementPlace(key: "n-1", strip: narrow, shows: "so.tilo.lang.tasks", title: "Tasks", shell: "so.tilo.shell.strips"))
    let wide = a_strip(a, row, "Board", 1000)
    a_foreign_place(a, wide, 1, "A terminal")
    let usual = a_strip(a, row, "Note", 667)
    a_note_place(a, usual, 1)
    let day_a = a_strip(a, row, "Morning", 667)
    let day_b = a_strip(a, row, "Afternoon", 667)
    let day_c = a_strip(a, row, "Evening", 667)
    let i = 0
    for n in notes.notes.take(12) {
        let held = if i < 4 { day_a } else if i < 8 { day_b } else { day_c }
        a.places.insert(ArrangementPlace(key: "d-{i}", strip: held, shows: "so.tilo.lang.notes", record: n.id(), title: n.title, shell: "so.tilo.shell.strips"))
        i += 1
    }
    v.focused = narrow
    a
}

/// A strip with nothing on it yet, beside a strip with one place: the state a person starts
/// from when they make a strip.
scenario "an empty strip" -> Arrangement {
    let v = viewing
    v.showing = .strips
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
    a_strip(a, row, "Empty", 667)
    let used = a_strip(a, row, "In use", 667)
    a_note_place(a, used, 0)
    a
}

/// A reference activated from a chat place: the channel's place, and the note's place it landed
/// beside — a column after the one it came from (shell.md §3).
scenario "a reference lands beside its place" -> Arrangement {
    let v = viewing
    v.showing = .strips
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
    let talks = a_strip(a, row, "Chat", 667)
    a.places.insert(ArrangementPlace(key: "ch", strip: talks, shows: "so.tilo.lang.chat", title: "design", shell: "so.tilo.shell.strips"))
    a_note_place(a, talks, 1)
    v.focused = talks
    a
}

/// Six places in two clusters on the framed canvas: one space per task, at three zoom levels as
/// the person looks closer or further out.
scenario "a canvas of spaces" -> Arrangement {
    let v = viewing
    v.showing = .frames
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
    let s = a_strip(a, row, "Canvas", 667)
    let strip = a.strips.find(st -> st.key == s)
    if let held = strip {
        held.canvas_x = 0
        held.canvas_y = 0
        held.canvas_width = 1100
        held.canvas_height = 1600
        held.placed = true
    }
    let i = 0
    for n in notes.notes.take(6) {
        let p = a.places.insert(ArrangementPlace(key: "c-{i}", strip: s, shows: "so.tilo.lang.notes", record: n.id(), title: n.title, shell: "so.tilo.shell.strips"))
        i += 1
    }
    a
}

/// The spaces canvas: the same places with no frame around them, so a resource's view stands as
/// content and nearness groups it.
scenario "resources standing as content" -> Arrangement {
    let v = viewing
    v.showing = .spaces
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
    let s = a_strip(a, row, "Spaces", 667)
    let i = 0
    for n in notes.notes.take(4) {
        a.places.insert(ArrangementPlace(key: "x-{i}", strip: s, shows: "so.tilo.lang.notes", record: n.id(), title: n.title, shell: "so.tilo.shell.strips"))
        i += 1
    }
    a
}
