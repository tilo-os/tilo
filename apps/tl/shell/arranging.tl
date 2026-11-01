/// Arranging: the actions that change the person's arrangement (shell.md §2). Each is one
/// change of their own record, attributed to whoever asked for it (the person's own input
/// included) and undone as that one step.
///
/// The arranging actions are the session's, published on `shell:arrangement` and listed in the
/// catalogue beside every app's (`crates/session/session/src/arrangement.rs`): open a place into
/// a strip, place a resource as one of its views at a canvas point, move a place into a row,
/// resize a strip, close a place, and pin a place. A person's
/// drag, the palette, a script and an agent all arrange through those same actions; this file is
/// where the shell's own views and keys call them. Nothing here changes the record itself: the
/// session owns it, so there is nothing a shell view could write behind anyone's back.
///
/// Each goes through the one capability the session takes for arranging, `session.arrange`,
/// whose payload names the published action and its arguments as one line of JSON.
///
/// Undo and redo of the arrangement are the record's own, on `shell:arrangement-record`.
/// Opens `shows` (an app's id) into the strip `strip`, after the place `after` or last, as the
/// new place `place`. What activating a reference does: a column after the one it came from
/// (§3), which the caller names by choosing the strip and the place to follow.
action open_into(strip: Text, place: Text, shows: Text, after: Text = "", shown_as: Text = "", record: Text = "") (icon: .plus, label: "Open beside") {
    require strip != "" "Name the strip to open into"
    require place != "" "Name the new place"
    require shows != "" "Name the app it shows"
    outside("session.arrange", "open-into \{\"strip\":\"{strip}\",\"place\":\"{place}\",\"shows\":\"{shows}\",\"after\":\"{after}\",\"view\":\"{shown_as}\",\"record\":\"{record}\"\}")
}

/// Puts the resource `text` finds on the canvas as its primary view (or `view`), standing at the
/// caret's canvas point `(x, y)`: what typing on the canvas does (shell.md §8, canvas.md §1).
/// The text runs the one `find` the palette and an agent's `tilo` tool use, so a search here
/// and a search there are the same search; the session resolves it into an entry naming the
/// resource and its view, attributed and undoable, and `shows` follows from the resource's app.
action place_found(text: Text, x: Int, y: Int, shown_as: Text = "") (icon: .map_pin, label: "Place what you find") {
    require text != "" "Type something to find and place"
    outside("session.arrange", "place-found \{\"text\":\"{text}\",\"x\":{x},\"y\":{y},\"view\":\"{shown_as}\"\}")
}

/// Puts the resource `resource` (its id, as a step printed it) on the canvas of the workspace in
/// front as its primary view (or `shown_as`), standing at the canvas point `(x, y)` in a strip of
/// its own: what activating a reference does (shell.md §3, §8.1). The session takes the
/// workspace, the row and the new ids from the surface in front, so the record holds them as for
/// any other place.
action place(a: Arrangement, resource: Text, x: Int, y: Int, shown_as: Text = "") (icon: .map_pin, label: "Place on the canvas") {
    require resource != "" "Name the resource to place"
    outside("session.arrange", "place \{\"resource\":\"{resource}\",\"x\":{x},\"y\":{y},\"view\":\"{shown_as}\"\}")
}

/// Moves the place `place` into the row `row`, before the place `before` or last: within a row,
/// or to another row, which is how a line of work moves whole (§3).
action move_place(place: Text, row: Text, before: Text = "") (icon: .navigation, label: "Move", group: "Arrange") {
    require place != "" "Name the place to move"
    require row != "" "Name the row it goes into"
    outside("session.arrange", "move-place \{\"place\":\"{place}\",\"row\":\"{row}\",\"before\":\"{before}\"\}")
}

/// Resizes the strip `strip` to `permille` of its row's usable width (333 narrow, 667 normal,
/// 1000 wide), as the person drags its edge.
action resize_strip(strip: Text, permille: Int) (icon: .columns_2, label: "Width", group: "Arrange") {
    require strip != "" "Name the strip to resize"
    require permille > 0 "A strip is wider than nothing"
    outside("session.arrange", "resize-strip \{\"strip\":\"{strip}\",\"permille\":{permille}\}")
}

/// Closes the place `place`: its window goes with it. The resource stays where it lives, and
/// nobody else's arrangement changes (§4).
destructive action close_place(place: Text) (icon: .x, label: "Close", group: "Arrange") {
    require place != "" "Name the place to close"
    outside("session.arrange", "close-place \{\"place\":\"{place}\"\}")
}

/// Pins the place `place`, so an agent's arrangement leaves it alone (§5, "Moving what the
/// person placed"), or unpins it.
action pin_place(place: Text, pinned: Bool = true) (icon: .pin, label: "Pin", group: "Arrange") {
    require place != "" "Name the place to pin"
    outside("session.arrange", "pin-place \{\"place\":\"{place}\",\"pinned\":{pinned}\}")
}

/// Steps back one arranging change: the person's own undo of their desk, as every change is
/// attributed.
action undo_arrangement(a: Arrangement) (icon: .undo_2, label: "Undo") {
    outside("session.arrange", "undo")
}

/// Steps forward again, after an undo.
action redo_arrangement(a: Arrangement) (icon: .redo_2, label: "Redo") {
    outside("session.arrange", "redo")
}
